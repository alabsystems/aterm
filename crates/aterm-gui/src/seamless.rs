// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live seamless-update handoff (proof-carrying DSU rung 1b).
//!
//! The outgoing process keeps every PTY master `FD_CLOEXEC`, makes a child-owned
//! `F_DUPFD_CLOEXEC` duplicate, and clears CLOEXEC only on the fork child's copy in `pre_exec`,
//! so it survives that one `execve` (proven with real syscalls in `aterm-pty`).
//! [`write_outgoing`] writes a nonce-authenticated [`SessionHandoff`] manifest into the per-user
//! `0700` control dir and returns the env the re-exec carries. The incoming process, at the top
//! of `main` while still single-threaded, calls [`take_incoming_from`]: it authenticates and
//! consumes the manifest, joins it to the volatile `(fd, pid)` channel ([`HandoffFds`],
//! `ATERM_SEAMLESS_FDS`), and yields the [`Adopted`] shells, each re-attached to an engine and
//! reader (`spawn_session(.., Some(adopted))`) instead of forked fresh.
//!
//! Fail-closed throughout: a missing, mismatched, wrong-dir or replayed manifest, a bad fd entry
//! or a non-tty fd falls back to a fresh spawn, so a spoofed `ATERM_SEAMLESS_*` can only lose an
//! adoption, never fabricate a session or adopt a stranger's fd. `HandoffEnv::capture`
//! (`crate::handoff_env`) takes every handoff name out of the environment once, before any
//! thread, so none leaks into the shell's children; the intakes read that snapshot.

use crate::handoff_env::{HandoffEnv, HandoffName};
use crate::session_store::{ConnectionCarry, HandoffFds, ScreenCarry, SessionHandoff, WindowCarry};
use crate::spawn::Adopted;
use aterm_core::terminal::{CheckpointMeta, TerminalCheckpoint};
use aterm_digest::Sha256;
use aterm_session::{LaunchNonce, SessionId};

pub(crate) const ENV_MANIFEST: HandoffName = HandoffName::new("ATERM_SEAMLESS_MANIFEST");
pub(crate) const ENV_FDS: HandoffName = HandoffName::new("ATERM_SEAMLESS_FDS");
pub(crate) const ENV_NONCE: HandoffName = HandoffName::new("ATERM_SEAMLESS_NONCE");
pub(crate) const ENV_LAYOUT: HandoffName = HandoffName::new("ATERM_SEAMLESS_LAYOUT");
pub(crate) const ENV_TARGET: HandoffName = HandoffName::new("ATERM_SEAMLESS_TARGET");

#[cfg(any(unix, test))]
const ADOPTION_PROOF_DOMAIN: &[u8] = b"aterm-seamless-adoption-v1\0";
const LAYOUT_PROOF_DOMAIN: &[u8] = b"aterm-seamless-layout-v1\0";
const SCREEN_PROOF_DOMAIN: &[u8] = b"aterm-seamless-screen-v1\0";
pub(crate) const MAX_HANDOFF_SESSIONS: usize = 256;
const MAX_HANDOFF_FDS_WIRE_BYTES: usize = 32 * 1024;
/// The largest manifest this build writes: the cap every shipped successor (v0.100.0 and older,
/// a rollback's target included) reads with, so frozen. This build reads up to
/// [`MAX_INCOMING_MANIFEST_BYTES`].
#[cfg(unix)]
const MAX_HANDOFF_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
/// The largest manifest this build reads: eight times what it writes (L3: the consumer is only
/// ever more lenient). Producers through v0.98.0 put no budget on the screen metas, so a desk
/// with much OSC 133 history (about 870 bytes per command) writes past 4 MiB. 32 MiB, the layout
/// sidecar's bound ([`MAX_HANDOFF_LAYOUT_BYTES`]), is some 38,000 commands, and the read is still
/// bounded before anything is allocated or parsed.
const MAX_INCOMING_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
/// What the handed sessions' screen metas may cost the manifest together, as its TOML writes them
/// ([`meta_manifest_cost`]): three quarters of [`MAX_HANDOFF_MANIFEST_BYTES`], leaving the rest
/// for the session records, the window carry and the held panes. The metas ride inline (the grids
/// go to sidecars) and a manifest past the cap is refused whole, so [`settle_wire_carries`]
/// lowers the largest metas until they fit.
#[cfg(any(unix, test))]
const MAX_HANDOFF_AGGREGATE_META_BYTES: u64 = 3 * 1024 * 1024;
/// The largest layout sidecar a successor reads; `handoff_diet`'s tests read
/// one the same way.
pub(crate) const MAX_HANDOFF_LAYOUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_HANDOFF_GRID_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HANDOFF_AGGREGATE_GRID_BYTES: u64 = 256 * 1024 * 1024;
/// Ceiling on one grid's DECODED cells (`rows * cols`) — a sanity bound on an
/// absurd geometry, charged before `restore_grid` constructs any `Cell`
/// vectors (the line codec can represent a very wide blank grid in a tiny
/// wire, so encoded bytes alone cannot bound it).
///
/// Law L3: this may only grow. The producer is the older build, so lowering it would refuse what
/// builds in the field send; a downgrade target is served by [`WireCaps::for_target`].
const MAX_HANDOFF_GRID_CELLS: u64 = MAX_HANDOFF_AGGREGATE_GRID_CELLS / 2;

/// The per-grid cell ceiling every consumer before
/// [`RELAXED_CONSUMER_FROM_BUILD`] enforced, frozen: what a producer must
/// honour when the successor it hands to is one of those (a rollback),
/// because that consumer's cap cannot be raised from here. See
/// [`WireCaps::for_target`].
#[cfg(any(unix, test))]
const LEGACY_HANDOFF_GRID_CELLS: u64 = 32 * 1024;

/// The first build whose consumer enforces this build's per-grid cap
/// ([`MAX_HANDOFF_GRID_CELLS`]) and bounds a line's attrs by its character
/// count: v0.93.0's claim in `RELEASES.ledger`. v0.92.0 (1790278596) and
/// every build before it enforce [`WireCaps::legacy`]'s; c6626970b3 and
/// 4f46175a29, which relaxed both, landed after it was cut. A development
/// build's number is its HEAD's commit time, so one built from a branch that
/// predates them but committed later is priced as v0.93.0 is.
#[cfg(any(unix, test))]
const RELAXED_CONSUMER_FROM_BUILD: u64 = 1_790_305_290;
/// Maximum scrollback lines a handoff checkpoint may carry, per session.
///
/// 256, not the whole ring: `dimension_grid_cap` prices a line generously (512 bytes per cell
/// plus 16 KiB framing) so a hostile meta cannot authorize a huge decode, and that multiplies per
/// carried line. Older history rides the history sidecar (`crate::handoff_history`), exported
/// with the readers live and imported after Commit, so this bounds only what the freeze pays
/// for. The producer may carry fewer lines, or none.
const MAX_HANDOFF_HISTORY_LINES: u32 = 256;

/// The producer's per-session history target: the wire ceiling, so a healthy capture carries
/// all the protocol allows.
#[must_use]
#[cfg(unix)]
pub(crate) fn max_handoff_history_lines() -> u32 {
    MAX_HANDOFF_HISTORY_LINES
}

/// Ceiling on decoded grid cells summed across every session in one handoff, charged by
/// [`admit_checkpoint_dimensions`] at all four seams (UI pre-capture, outgoing digest, outgoing
/// write, incoming decode) so they agree on what a pool costs. A session costs
/// `cols * (2 * rows + history)` over every tab and pane; 4 Mi cells (32 MiB at 8 bytes a
/// `Cell`) fits an ordinary desk and stays well inside the encoded byte caps. The producer
/// (`app_update_handoff`) reserves every session's visible+alt cells before any session spends
/// the aggregate on scrollback.
const MAX_HANDOFF_AGGREGATE_GRID_CELLS: u64 = 4 * 1024 * 1024;
#[cfg(unix)]
const READY_WIRE_MAGIC: &[u8; 4] = b"ASR1";
#[cfg(unix)]
const COMMIT_WIRE_MAGIC: &[u8; 4] = b"ASC1";
#[cfg(unix)]
pub(crate) const READY_WIRE_LEN: usize = 4 + 4 + 32;

/// Which handoff env names are present: one named set, because the shape rule reads all eight
/// and eight positional booleans would hide a transposed pair at the call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(unix, test))]
struct HandoffEnvPresence {
    manifest: bool,
    fds: bool,
    nonce: bool,
    layout: bool,
    ready: bool,
    commit: bool,
    rendezvous: bool,
    claim: bool,
}

#[cfg(any(unix, test))]
fn handoff_is_modern_overlap(present: HandoffEnvPresence) -> bool {
    let HandoffEnvPresence {
        manifest,
        fds,
        nonce,
        layout,
        ready,
        commit,
        rendezvous,
        claim,
    } = present;
    if !(manifest && nonce && layout) {
        return false;
    }
    let inherited = fds && ready && commit && !rendezvous && !claim;
    // macOS-only because the launcher is: on every other platform the out-of-band
    // shape can only be a forgery or a bug, and is refused as one.
    let out_of_band = cfg!(target_os = "macos") && rendezvous && claim && !fds && !ready && !commit;
    inherited || out_of_band
}

/// One adopted session's identity on the wire: pool-local session id, inherited PTY master fd
/// and shell pid, in that order. It is the unit the fd channel carries, [`validated_identities`]
/// matches against the manifest and [`adoption_proof`] hashes, so the three agree on field order.
/// The fd is a transport coordinate, not an identity: a slot in one process's descriptor table
/// that both sides share only because `fork`+`execve` copies it (see [`adoption_proof`]).
pub(crate) type SessionIdentity = (u64, i32, i32);

/// Exact, attempt-bound proof of the PTYs the incoming process really adopted.
/// The nonce binds it to one outgoing manifest; the sorted identity triples bind
/// both cardinality and membership, so zero/subset/duplicate adoption cannot be
/// mistaken for complete readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(unix, test))]
pub(crate) struct AdoptionProof {
    count: u32,
    digest: [u8; 32],
}

#[cfg(any(unix, test))]
impl AdoptionProof {
    #[cfg(unix)]
    fn to_wire_with_magic(self, magic: &[u8; 4]) -> [u8; READY_WIRE_LEN] {
        let mut wire = [0u8; READY_WIRE_LEN];
        wire[..4].copy_from_slice(magic);
        wire[4..8].copy_from_slice(&self.count.to_be_bytes());
        wire[8..].copy_from_slice(&self.digest);
        wire
    }

    #[must_use]
    #[cfg(unix)]
    pub(crate) fn to_wire(self) -> [u8; READY_WIRE_LEN] {
        self.to_wire_with_magic(READY_WIRE_MAGIC)
    }

    #[must_use]
    #[cfg(unix)]
    pub(crate) fn to_commit_wire(self) -> [u8; READY_WIRE_LEN] {
        self.to_wire_with_magic(COMMIT_WIRE_MAGIC)
    }

    #[must_use]
    #[cfg(unix)]
    pub(crate) fn from_wire(wire: &[u8; READY_WIRE_LEN]) -> Option<Self> {
        Self::from_wire_with_magic(wire, READY_WIRE_MAGIC)
    }

    #[cfg(unix)]
    fn from_wire_with_magic(wire: &[u8; READY_WIRE_LEN], magic: &[u8; 4]) -> Option<Self> {
        if &wire[..4] != magic {
            return None;
        }
        let count = u32::from_be_bytes(wire[4..8].try_into().ok()?);
        let digest = wire[8..].try_into().ok()?;
        Some(Self { count, digest })
    }

    #[must_use]
    #[cfg(unix)]
    pub(crate) fn commit_wire_matches(self, wire: &[u8; READY_WIRE_LEN]) -> bool {
        Self::from_wire_with_magic(wire, COMMIT_WIRE_MAGIC) == Some(self)
    }
}

/// Canonical SHA-256 commitment to one complete adopted session set: the attempt nonce, the
/// authorized target build/commit, the layout and screen digests, and the sorted
/// [`SessionIdentity`] triples. Sorting makes it order-free; hashing the count beside the members
/// makes zero/subset/duplicate adoption unmistakable.
///
/// Frozen cross-version wire: the parent runs the outgoing build's copy, the child the incoming
/// build's, and nothing negotiates the format. A disagreement is an `AdoptionMismatch`, which
/// `finish_update_handoff` latches manual-only, so changing any hashed byte retires the automatic
/// update lane for every parent in the field.
/// `the_v1_adoption_digest_is_frozen_to_this_exact_vector` pins one input to its exact digest
/// (domain, field order, length prefixes, sort, and [`normalize_commit`]'s folding);
/// `f4_adoption_proof_asymmetry` records the last drift.
///
/// That sentence was, until now, only a sentence: every test here compared this
/// function against ITSELF, so a reviewer who changed a hashed byte for some
/// unrelated reason would have found the suite entirely green and the field
/// entirely broken. `the_v1_adoption_digest_is_frozen_to_this_exact_vector`
/// below is the tripwire — one hardcoded 32-byte expectation for one fixed
/// input, which is what makes an edit to this function a red test instead of a
/// fleet-wide update outage. It pins the domain string, the field order, the
/// length prefixes, the sort, and [`normalize_commit`]'s truncation and
/// case-folding, because all of those are hashed bytes too.
///
/// WHAT THE FD TERM ACTUALLY PROVES. A descriptor number is not a property of a
/// PTY; it is the slot the PTY occupies in one process's descriptor table. Both
/// sides compute the same value only because `fork`+`execve` copies that table
/// verbatim, so the child's inherited master keeps the parent's number, and our
/// child never relocates these descriptors. Under that transport the term does
/// exclude a substituted or partial adoption — but the exclusion is supplied by
/// the transport plus our own code, and merely WITNESSED by the digest, which
/// checks an integer that any process may put on any descriptor with `dup2`.
/// Move the masters to `SCM_RIGHTS` (B4) and the
/// kernel picks the receiver's numbers: the term stops agreeing, and the only
/// way to keep it would be `dup2` surgery onto parent-chosen numbers at process
/// entry, where one mistake destroys a live descriptor.
///
/// B4 — THE REPLACEMENT TERM. What survives a descriptor transfer is the PTY
/// ITSELF. On macOS that is `fstat(2)`'s `st_rdev` on the master: `/dev/ptmx` is
/// a cloning device, so every open takes its own minor, that minor is the
/// `/dev/ttysNNN` its slave gets, and `dup`/`SCM_RIGHTS` hand over the same open
/// file description and therefore the same value (observed: three
/// simultaneously-open masters at minors 3/98/119, each matching its slave's
/// name, `dup` preserving the value). `st_ino`/`st_dev` are NOT usable there —
/// every master shares the single `/dev/ptmx` devfs node. Linux exposes the same
/// number as `ioctl(fd, TIOCGPTN)`. That term is strictly stronger than the fd
/// number: a same-uid process cannot make `fstat` lie about a descriptor the
/// caller already holds, and to ANSWER a given minor it has to hold that very
/// PTY — minting a character device with a chosen `st_rdev` needs `mknod(2)`,
/// i.e. root. The "descriptor's ordinal in the parent-declared transfer order"
/// is transport-portable but as
/// content-free as the number: it names a slot, so it detects a permutation and
/// never a substitution. It is fine as ADDRESSING — which received descriptor
/// is which `local_id` — and useless as proof.
///
/// The code that relies on that paragraph asserts it:
/// `handoff_rendezvous::pty_device_term`'s
/// `the_device_term_distinguishes_masters` (three real masters, three DISTINCT
/// terms) and `a_grant_whose_connection_closes_at_once_is_still_claimed` (the
/// `SCM_RIGHTS` half: over a real transfer, the same PTY at a different number
/// keeps its term).
///
/// It is not landed here because it IS the frozen-wire change above. The plan
/// used to be a version ADVERTISEMENT through [`outgoing_parent_env`] — parent
/// advertises v2 when its authorized `target_build` is at least its own build,
/// child emits v2 only when advertised. That is retired, and the reason is worth
/// keeping: an advertisement is a second thing that can be mis-gated, and the
/// cost of mis-gating THIS one is not one broken machine but `AdoptionMismatch`
/// on every parent in the field, which `finish_update_handoff` latches
/// manual-only. It also buys nothing, because the TRANSPORT already decides the
/// term with no negotiation at all: a parent can only put descriptors out of
/// band if it has out-of-band transport code, and a parent already in the field
/// does not — it sends `ATERM_SEAMLESS_FDS` and is answered in v1, exactly as
/// today. Presence of the transport IS the version, which makes "no parent in
/// the field can ever be shown v2" a structural fact instead of an argument
/// about gating. What has not changed is that the swap cannot be made from this
/// file alone: the transport that selects the term lives in
/// `app_update_handoff`, and the term follows it.
///
/// The advertisement did carry one guard worth keeping, and retiring it must not
/// drop it: "presence of the transport IS the version" settles old-parent /
/// new-child and says nothing about new-parent / old-child. So the guard moves to
/// the transport CHOICE — a parent selects the out-of-band lane only when its
/// authorized `target_build` is at least its own build (`job.target_build` is
/// already in scope in `run_handoff_worker`), and forks otherwise, so an older
/// successor is never handed descriptors it has no code to receive.
///
/// A REFUSAL NAMES ITS INPUT (warm successor P6, finding 2): an unstamped
/// build's `unknown` commit used to come back as a bare `None`, which the
/// outgoing process reported as "handoff identity set exceeds the proof
/// format" — the session count's words over a refusal of the commit.
#[cfg(any(unix, test))]
pub(crate) fn adoption_proof(
    nonce: &str,
    target_build: u64,
    target_commit: &str,
    layout_digest: &[u8; 32],
    screen_digest: &[u8; 32],
    identities: &[SessionIdentity],
) -> Result<AdoptionProof, ProofRefusal> {
    let count =
        u32::try_from(identities.len()).map_err(|_| ProofRefusal::Sessions(identities.len()))?;
    let nonce_len = u32::try_from(nonce.len()).map_err(|_| ProofRefusal::Nonce(nonce.len()))?;
    let commit_prefix = handoff_commit_stamp(target_commit)?;
    // At most 12 hex digits and `-dirty` (`normalize_commit`).
    let commit_len = u32::try_from(commit_prefix.len())
        .map_err(|_| ProofRefusal::UnstampedCommit(target_commit.trim().to_string()))?;
    let mut identities = identities.to_vec();
    identities.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(ADOPTION_PROOF_DOMAIN);
    hasher.update(nonce_len.to_be_bytes());
    hasher.update(nonce.as_bytes());
    hasher.update(target_build.to_be_bytes());
    hasher.update(commit_len.to_be_bytes());
    hasher.update(commit_prefix.as_bytes());
    hasher.update(layout_digest);
    hasher.update(screen_digest);
    hasher.update(count.to_be_bytes());
    for (local_id, master, pid) in identities {
        hasher.update(local_id.to_be_bytes());
        hasher.update(master.to_be_bytes());
        hasher.update(pid.to_be_bytes());
    }
    Ok(AdoptionProof {
        count,
        digest: hasher.finalize(),
    })
}

/// Why [`adoption_proof`] formed no proof: which input the frozen wire cannot
/// carry, each said by its own cause.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(unix, test))]
pub(crate) enum ProofRefusal {
    /// More handed sessions than the wire's `u32` count holds.
    Sessions(usize),
    /// A nonce longer than the wire's `u32` length prefix holds.
    Nonce(usize),
    /// A commit that is no build stamp [`normalize_commit`] admits: in a
    /// release build, the `unknown` of a build made without its git facts.
    UnstampedCommit(String),
}

#[cfg(any(unix, test))]
impl std::fmt::Display for ProofRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sessions(n) => write!(
                f,
                "handoff identity set of {n} sessions exceeds the proof format"
            ),
            Self::Nonce(n) => write!(f, "a {n}-byte handoff nonce exceeds the proof format"),
            Self::UnstampedCommit(commit) => write!(
                f,
                "the target build's commit reads `{commit}`, which is no commit stamp, and \
                 the adoption proof names the successor's commit: a build made without its \
                 commit stamp (ATERM_BUILD_GIT_COMMIT) cannot take a seamless handoff"
            ),
        }
    }
}

/// [`normalize_commit`] with its refusal named: the spelling the adoption proof hashes, or
/// [`ProofRefusal::UnstampedCommit`]. The outgoing process asks this before it launches a
/// successor or parks a reader (`App::start_unix_update_handoff`), so a build that can never
/// prove is refused there, by its cause, with the terminal untouched.
#[cfg(any(unix, test))]
pub(crate) fn handoff_commit_stamp(commit: &str) -> Result<String, ProofRefusal> {
    normalize_commit(commit).ok_or_else(|| ProofRefusal::UnstampedCommit(commit.trim().to_string()))
}

/// The one canonical spelling of a build's git commit, shared by every side of the handoff. The
/// parent's expectation (from a release ticket, possibly a full 40-hex sha) and the child's
/// compiled-in `build_info::git_commit()` (12 hex, optionally `-dirty`) must reduce to the same
/// string, or the two describe different binaries; the child checks that equality by name
/// ([`take_target_identity_from`]) rather than leaving it to the digest.
#[must_use]
pub(crate) fn normalize_commit(commit: &str) -> Option<String> {
    let commit = commit.trim().to_ascii_lowercase();
    let (base, dirty) = commit
        .strip_suffix("-dirty")
        .map_or((commit.as_str(), false), |base| (base, true));
    if base.as_bytes().iter().all(u8::is_ascii_hexdigit) && base.len() >= 7 {
        let mut normalized = base[..base.len().min(12)].to_string();
        if dirty {
            normalized.push_str("-dirty");
        }
        return Some(normalized);
    }
    // `unknown` is a debug/dev shape and must never authorize a release handoff, so only
    // `debug_assertions` (a property of the binary, not of the launching shell) admits it.
    // `ATERM_DEBUG_SEAMLESS_REEXEC` does not relax this: QA against an unstamped build needs a
    // stamped throwaway commit.
    (base == "unknown" && !dirty && cfg!(debug_assertions)).then(|| "unknown".to_string())
}

/// Canonical, attempt-bound commitment to the complete window/tab/pane layout.
///
/// Byte-identity law: a pure function of the wire the parent writes, and both sides hash those
/// same bytes. The parent writes exactly `layout.to_toml()` to the layout sidecar
/// (`handoff_diet::seal_layout`) and hashes it here, and `prepare_outgoing_artifacts` checks the
/// two agree; the child hashes the bytes it read ([`layout_wire_digest`]), never a
/// re-serialization of its parse, since a new build's TOML codec is no byte fixed point on an old
/// build's wire.
#[must_use]
#[cfg(any(unix, test))]
pub(crate) fn layout_digest(layout: &crate::restore::RestoreManifest) -> Option<[u8; 32]> {
    layout_wire_digest(&layout.to_toml().ok()?)
}

/// [`layout_digest`] keyed on the exact serialized bytes. This is what the
/// child uses, over the bytes it consumed from the parent's sidecar.
#[must_use]
pub(crate) fn layout_wire_digest(wire: &str) -> Option<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(LAYOUT_PROOF_DOMAIN);
    hasher.update(u64::try_from(wire.len()).ok()?.to_be_bytes());
    hasher.update(wire.as_bytes());
    Some(hasher.finalize())
}

/// Which of a checkpoint's two grid blobs a refusal is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GridSlot {
    /// The active grid: the one `history_lines` describes.
    Main,
    /// The inactive grid (`alt_grid`): the saved primary while the alternate screen is up. The
    /// wire allows it exactly `rows` records.
    Inactive,
}

impl std::fmt::Display for GridSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Main => "main grid",
            Self::Inactive => "inactive grid",
        })
    }
}

/// Why a visible-checkpoint set could not be committed canonically. Diagnostic only: no variant
/// is hashed ([`screen_wire_digest`] is the sole hashing surface). Every arm names itself and the
/// session it blames, so a refused update says why.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(unix, test))]
pub(crate) enum ScreenDigestRefusal {
    TooManySessions {
        count: usize,
        max: usize,
    },
    DuplicateLocalId {
        local_id: u64,
    },
    Alloc {
        what: &'static str,
        count: usize,
    },
    MetaUnbounded {
        local_id: u64,
        rows: u16,
        cols: u16,
        history_lines: u32,
        /// The first bound the meta broke, named. `None` only when every named
        /// bound held and the carried line count alone is over the handoff
        /// maximum (`dimension_grid_cap`).
        violation: Option<MetaBoundViolation>,
    },
    DimensionsRefused {
        local_id: u64,
        rows: u16,
        cols: u16,
        history_lines: u32,
        has_alt: bool,
        /// Which ceiling refused it.
        bound: AdmitRefusal,
    },
    CapDisagreement {
        local_id: u64,
        admitted: u64,
        meta_cap: u64,
    },
    ParserNotGround {
        local_id: u64,
    },
    AltPairingMismatch {
        local_id: u64,
        has_grid: bool,
        has_cursor: bool,
    },
    GridNotCanonical {
        local_id: u64,
        which: GridSlot,
        rows: u16,
        cols: u16,
        expected_records: u64,
        bytes: usize,
    },
    GridOverCap {
        local_id: u64,
        which: GridSlot,
        len: u64,
        cap: u64,
    },
    LengthNotRepresentable {
        local_id: u64,
        which: GridSlot,
    },
    MetaSerialization {
        local_id: u64,
    },
    WireFraming,
    /// The screen metas cost the manifest more than
    /// [`MAX_HANDOFF_AGGREGATE_META_BYTES`] even with every carried shell mark
    /// shed ([`settle_wire_carries`]).
    MetasOverBudget {
        cost: u64,
        budget: u64,
    },
}

#[cfg(any(unix, test))]
impl ScreenDigestRefusal {
    /// The session this refusal blames, which [`settle_wire_carries`] lowers; `None` for the
    /// pool-wide arms (a duplicate id names an id, but as an identity fact).
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) fn local_id(&self) -> Option<u64> {
        match *self {
            Self::TooManySessions { .. }
            | Self::Alloc { .. }
            | Self::WireFraming
            | Self::MetasOverBudget { .. }
            | Self::DuplicateLocalId { .. } => None,
            Self::MetaUnbounded { local_id, .. }
            | Self::DimensionsRefused { local_id, .. }
            | Self::CapDisagreement { local_id, .. }
            | Self::ParserNotGround { local_id }
            | Self::AltPairingMismatch { local_id, .. }
            | Self::GridNotCanonical { local_id, .. }
            | Self::GridOverCap { local_id, .. }
            | Self::LengthNotRepresentable { local_id, .. }
            | Self::MetaSerialization { local_id } => Some(local_id),
        }
    }
}

#[cfg(any(unix, test))]
impl std::fmt::Display for ScreenDigestRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::TooManySessions { count, max } => {
                write!(
                    formatter,
                    "{count} sessions exceeds the {max}-session limit"
                )
            }
            Self::DuplicateLocalId { local_id } => {
                write!(formatter, "two checkpoints claim local id {local_id}")
            }
            Self::Alloc { what, count } => {
                write!(
                    formatter,
                    "could not reserve the {what} for {count} session(s)"
                )
            }
            Self::MetaUnbounded {
                local_id,
                rows,
                cols,
                history_lines,
                violation,
            } => {
                write!(
                    formatter,
                    "session {local_id}: meta out of bounds at {rows}x{cols} with {history_lines} carried line(s): "
                )?;
                match violation {
                    Some(violation) => write!(formatter, "{violation}"),
                    None => write!(
                        formatter,
                        "history_lines={history_lines} must be at most MAX_HANDOFF_HISTORY_LINES={MAX_HANDOFF_HISTORY_LINES}"
                    ),
                }
            }
            Self::DimensionsRefused {
                local_id,
                rows,
                cols,
                history_lines,
                has_alt,
                bound,
            } => write!(
                formatter,
                "session {local_id}: {rows}x{cols} with {history_lines} carried line(s) \
                 (alt={has_alt}) was refused: {bound}"
            ),
            Self::CapDisagreement {
                local_id,
                admitted,
                meta_cap,
            } => write!(
                formatter,
                "session {local_id}: admission priced the grid at {admitted} bytes but the meta caps it at {meta_cap}"
            ),
            Self::ParserNotGround { local_id } => {
                write!(
                    formatter,
                    "session {local_id}: parser was not in Ground state"
                )
            }
            Self::AltPairingMismatch {
                local_id,
                has_grid,
                has_cursor,
            } => write!(
                formatter,
                "session {local_id}: inactive grid/cursor disagree (grid={has_grid}, cursor={has_cursor})"
            ),
            Self::GridNotCanonical {
                local_id,
                which,
                rows,
                cols,
                expected_records,
                bytes,
            } => write!(
                formatter,
                "session {local_id}: {which} is not canonical — {bytes} byte(s) must decode as exactly {expected_records} record(s) at {rows}x{cols} and re-encode identically"
            ),
            Self::GridOverCap {
                local_id,
                which,
                len,
                cap,
            } => write!(
                formatter,
                "session {local_id}: {which} is {len} byte(s), over its {cap}-byte cap"
            ),
            Self::LengthNotRepresentable { local_id, which } => write!(
                formatter,
                "session {local_id}: {which} length is not representable as u64"
            ),
            Self::MetaSerialization { local_id } => {
                write!(formatter, "session {local_id}: meta would not serialize")
            }
            Self::WireFraming => {
                formatter.write_str("the wire entries could not be framed for hashing")
            }
            Self::MetasOverBudget { cost, budget } => write!(
                formatter,
                "the screen metas cost the manifest {cost} bytes, over its {budget}-byte budget, \
                 with every carried shell mark shed"
            ),
        }
    }
}

/// The `cap`-independent half of what [`screen_digest_and_meta_cost`] demands of one checkpoint:
/// the parser was Ground, the inactive grid and its cursor agree, and both grid blobs hold exactly
/// the record count the wire declares and re-encode to the same bytes.
///
/// Shared with the producer's ladder ([`carry_for_wire`]) so the two cannot disagree on shape,
/// and split here because these are the predicates a producer can still act on: each turns on the
/// carry bound and the geometry, both of which it can choose again. The `cap` checks stay in the
/// digest, beside the meta that sets the cap.
#[must_use]
#[cfg(any(unix, test))]
pub(crate) fn checkpoint_shape_refusal(
    local_id: u64,
    checkpoint: &TerminalCheckpoint,
) -> Option<ScreenDigestRefusal> {
    // A carried parser (`checkpoint.parser`) is not Ground and is not refused: the successor
    // continues it. Refused is a parser left mid-sequence with nothing carried.
    if !checkpoint.parser_ground && checkpoint.parser.is_none() {
        return Some(ScreenDigestRefusal::ParserNotGround { local_id });
    }
    if checkpoint.alt_grid.is_some() != checkpoint.alt_cursor.is_some() {
        return Some(ScreenDigestRefusal::AltPairingMismatch {
            local_id,
            has_grid: checkpoint.alt_grid.is_some(),
            has_cursor: checkpoint.alt_cursor.is_some(),
        });
    }
    if !checkpoint_grid_is_canonical(
        &checkpoint.grid,
        checkpoint.rows,
        checkpoint.cols,
        checkpoint.history_lines,
    ) {
        return Some(ScreenDigestRefusal::GridNotCanonical {
            local_id,
            which: GridSlot::Main,
            rows: checkpoint.rows,
            cols: checkpoint.cols,
            expected_records: u64::from(checkpoint.rows) + u64::from(checkpoint.history_lines),
            bytes: checkpoint.grid.len(),
        });
    }
    // The inactive grid's blob is always exactly `rows` records: the wire's `history_lines`
    // describes the main blob, so nothing can declare a history for this one.
    if let Some(alt) = checkpoint.alt_grid.as_ref()
        && !checkpoint_grid_is_canonical(alt, checkpoint.rows, checkpoint.cols, 0)
    {
        return Some(ScreenDigestRefusal::GridNotCanonical {
            local_id,
            which: GridSlot::Inactive,
            rows: checkpoint.rows,
            cols: checkpoint.cols,
            expected_records: u64::from(checkpoint.rows),
            bytes: alt.len(),
        });
    }
    None
}

/// Canonical commitment to every required visible-screen checkpoint: sorted by local id, length
/// framed, over canonical wire encodings only (non-canonical ones are refused before hashing).
/// Test-only: the capture commits through [`settle_wire_carries`], which runs the same predicate
/// ([`screen_digest_and_meta_cost`]) and lowers whatever it blames.
#[cfg(test)]
pub(crate) fn screen_digest(
    screens: &[(u64, TerminalCheckpoint)],
) -> Result<[u8; 32], ScreenDigestRefusal> {
    screen_digest_refs(
        screens
            .iter()
            .map(|(local_id, checkpoint)| (*local_id, checkpoint))
            .collect(),
    )
}

/// [`screen_digest_and_meta_cost`]'s digest alone.
#[cfg(test)]
fn screen_digest_refs(
    screens: Vec<(u64, &TerminalCheckpoint)>,
) -> Result<[u8; 32], ScreenDigestRefusal> {
    screen_digest_and_meta_cost(screens).map(|(digest, _)| digest)
}

/// The screen commitment over `screens`, refusing by name in a fixed short-circuit order, with
/// what the hashed metas cost the manifest together ([`meta_manifest_cost`]), priced over the
/// bytes it serialized so the self-check's meta budget needs no second serialization.
#[cfg(any(unix, test))]
fn screen_digest_and_meta_cost(
    mut screens: Vec<(u64, &TerminalCheckpoint)>,
) -> Result<([u8; 32], u64), ScreenDigestRefusal> {
    if screens.len() > MAX_HANDOFF_SESSIONS {
        return Err(ScreenDigestRefusal::TooManySessions {
            count: screens.len(),
            max: MAX_HANDOFF_SESSIONS,
        });
    }
    screens.sort_unstable_by_key(|(local_id, _)| *local_id);
    if let Some(pair) = screens.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(ScreenDigestRefusal::DuplicateLocalId {
            local_id: pair[0].0,
        });
    }
    let mut aggregate_cells = 0_u64;
    // Project each checkpoint into exactly the bytes `write_outgoing` puts on the wire
    // (`aterm_json` meta + the verbatim grid blobs) and hash those. The child hashes the same
    // bytes after reading them back, so the digests agree by construction.
    let mut metas = Vec::new();
    metas
        .try_reserve_exact(screens.len())
        .map_err(|_| ScreenDigestRefusal::Alloc {
            what: "meta table",
            count: screens.len(),
        })?;
    for (local_id, checkpoint) in &screens {
        let local_id = *local_id;
        let meta = CheckpointMeta::from_checkpoint(checkpoint);
        let cap = checkpoint_grid_cap(&meta).ok_or_else(|| ScreenDigestRefusal::MetaUnbounded {
            local_id,
            rows: checkpoint.rows,
            cols: checkpoint.cols,
            history_lines: checkpoint.history_lines,
            violation: checkpoint_meta_bound_violation(&meta),
        })?;
        let admitted = admit_checkpoint_dimensions(
            &mut aggregate_cells,
            checkpoint.rows,
            checkpoint.cols,
            checkpoint.history_lines,
            checkpoint.alt_grid.is_some(),
        )
        .map_err(|bound| ScreenDigestRefusal::DimensionsRefused {
            local_id,
            rows: checkpoint.rows,
            cols: checkpoint.cols,
            history_lines: checkpoint.history_lines,
            has_alt: checkpoint.alt_grid.is_some(),
            bound,
        })?;
        if admitted != cap {
            return Err(ScreenDigestRefusal::CapDisagreement {
                local_id,
                admitted,
                meta_cap: cap,
            });
        }
        // The shape half, shared with the producer. It runs before the two `cap` comparisons
        // below, which changes only which refusal a multi-fault checkpoint reports.
        if let Some(refusal) = checkpoint_shape_refusal(local_id, checkpoint) {
            return Err(refusal);
        }
        let grid_len = u64::try_from(checkpoint.grid.len()).map_err(|_| {
            ScreenDigestRefusal::LengthNotRepresentable {
                local_id,
                which: GridSlot::Main,
            }
        })?;
        if grid_len > cap {
            return Err(ScreenDigestRefusal::GridOverCap {
                local_id,
                which: GridSlot::Main,
                len: grid_len,
                cap,
            });
        }
        // A length that does not fit a `u64` is not compared to the cap: the shape check above
        // already refuses such a blob.
        if let Some(alt) = checkpoint.alt_grid.as_ref()
            && let Ok(alt_len) = u64::try_from(alt.len())
            && alt_len > cap
        {
            return Err(ScreenDigestRefusal::GridOverCap {
                local_id,
                which: GridSlot::Inactive,
                len: alt_len,
                cap,
            });
        }
        metas.push((
            local_id,
            aterm_json::to_vec(&meta)
                .map_err(|_| ScreenDigestRefusal::MetaSerialization { local_id })?,
        ));
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(screens.len())
        .map_err(|_| ScreenDigestRefusal::Alloc {
            what: "wire entry table",
            count: screens.len(),
        })?;
    let mut meta_cost = 0_u64;
    for ((local_id, meta), (_, checkpoint)) in metas.iter().zip(screens.iter()) {
        meta_cost = meta_cost.saturating_add(meta_json_manifest_cost(meta));
        entries.push(ScreenWireEntry {
            local_id: *local_id,
            meta,
            grid: &checkpoint.grid,
            alt_grid: checkpoint.alt_grid.as_deref(),
        });
    }
    screen_wire_digest(&mut entries)
        .map(|digest| (digest, meta_cost))
        .ok_or(ScreenDigestRefusal::WireFraming)
}

/// One session's screen carry as bytes: exactly what crosses the wire.
struct ScreenWireEntry<'a> {
    local_id: u64,
    /// The `aterm_json` `CheckpointMeta` blob (`ScreenCarry::meta`), verbatim.
    meta: &'a [u8],
    /// The main grid sidecar's `serialize_lines` bytes, verbatim.
    grid: &'a [u8],
    /// The alt grid sidecar's bytes when the carry declares one.
    alt_grid: Option<&'a [u8]>,
}

/// The canonical screen commitment. Sorting by local id removes capture-order ambiguity; length
/// framing prevents concatenation aliases. The outgoing process (over the bytes it is about to
/// write) and the incoming one (over the bytes it read) hash the same bytes, which is what lets
/// the adoption proof survive a version boundary.
fn screen_wire_digest(entries: &mut [ScreenWireEntry<'_>]) -> Option<[u8; 32]> {
    if entries.len() > MAX_HANDOFF_SESSIONS {
        return None;
    }
    entries.sort_unstable_by_key(|entry| entry.local_id);
    if entries
        .windows(2)
        .any(|pair| pair[0].local_id == pair[1].local_id)
    {
        return None;
    }
    let mut aggregate = 0_u64;
    let mut hasher = Sha256::new();
    hasher.update(SCREEN_PROOF_DOMAIN);
    hasher.update(u32::try_from(entries.len()).ok()?.to_be_bytes());
    for entry in entries {
        let grid_len = u64::try_from(entry.grid.len()).ok()?;
        aggregate = aggregate.checked_add(grid_len)?;
        hasher.update(entry.local_id.to_be_bytes());
        hasher.update(u64::try_from(entry.meta.len()).ok()?.to_be_bytes());
        hasher.update(entry.meta);
        hasher.update(grid_len.to_be_bytes());
        hasher.update(entry.grid);
        match entry.alt_grid {
            Some(alt) => {
                let alt_len = u64::try_from(alt.len()).ok()?;
                aggregate = aggregate.checked_add(alt_len)?;
                hasher.update([1]);
                hasher.update(alt_len.to_be_bytes());
                hasher.update(alt);
            }
            None => hasher.update([0]),
        }
        if aggregate > MAX_HANDOFF_AGGREGATE_GRID_BYTES {
            return None;
        }
    }
    Some(hasher.finalize())
}

/// 16 CSPRNG bytes as 32 lowercase-hex chars, for the single-use handoff nonce, from the
/// workspace's audited entropy surface ([`aterm_uds::rand::hex_token`]). Entropy failure degrades
/// to the all-zero token: a guessed nonce only costs the adoption (everything downstream fails
/// closed), never fabricates a session or adopts a stranger's fd.
#[cfg(unix)]
fn random_nonce() -> String {
    aterm_uds::rand::hex_token::<16>().unwrap_or_else(|_| "0".repeat(32))
}

/// Mint the attempt nonce an outgoing handoff is named by, before anything is written: the
/// successor is launched with the manifest's path in its environment before the manifest exists,
/// so the writer ([`write_outgoing`]) and the launch environment derive the same name from the
/// same nonce ([`outgoing_manifest_path`]).
#[cfg(unix)]
pub(crate) fn mint_outgoing_nonce() -> String {
    random_nonce()
}

/// The manifest's file name under the private control dir for `nonce`: the one
/// spelling both the writer and the launch environment use.
#[cfg(unix)]
fn outgoing_manifest_name(nonce: &str) -> String {
    format!("seamless-{}-{nonce}.toml", std::process::id())
}

/// Where [`write_outgoing`] will put the manifest for `nonce` — `None` when there
/// is no private control dir (then no seamless handoff is possible at all).
#[cfg(unix)]
pub(crate) fn outgoing_manifest_path(nonce: &str) -> Option<std::path::PathBuf> {
    Some(crate::control_auth::socket_dir()?.join(outgoing_manifest_name(nonce)))
}

/// Write `bytes` to `path` with owner-only permissions (the manifest's `0600` posture), keeping
/// the error: a full volume and a refused permission are different failures to file.
#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
}

/// Why [`write_outgoing`] wrote nothing. Every arm is this process's failure, never a verdict on
/// the candidate (so not `PreparationFailed`); the `std::io::ErrorKind` travels out so the
/// ledger can say `storage full`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(unix)]
pub(crate) enum WriteOutgoingFailure {
    /// No private control directory to write into.
    NoPrivateDir,
    /// The capture handed to the writer disagrees with itself (the session,
    /// screen and descriptor sets, a screen past a bound, a meta that will not
    /// serialize) — this build's own inconsistency.
    Inconsistent,
    /// The filesystem refused a write.
    Io(std::io::ErrorKind),
    /// The manifest would be past the cap the successor reads it under
    /// ([`MAX_HANDOFF_MANIFEST_BYTES`]) with every held pane shed: written, it
    /// would be refused whole, so it is not written.
    ManifestOverCap,
}

#[cfg(unix)]
impl std::fmt::Display for WriteOutgoingFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPrivateDir => formatter.write_str("no private control directory"),
            Self::Inconsistent => formatter.write_str("the capture disagreed with itself"),
            Self::Io(kind) => write!(formatter, "{kind}"),
            Self::ManifestOverCap => write!(
                formatter,
                "the open tabs' records are over the {MAX_HANDOFF_MANIFEST_BYTES}-byte limit \
                 one handover carries"
            ),
        }
    }
}

/// Create `path` afresh with owner-only permissions and write `bytes`:
/// `O_CREAT|O_EXCL|O_NOFOLLOW`, so nothing already there — a file, or a
/// symlink someone planted to steer the write — is ever written through. A
/// partial write is unlinked. `None` on any failure.
#[cfg(unix)]
fn write_private_new(path: &std::path::Path, bytes: &[u8]) -> Option<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path).ok()?;
    if file.write_all(bytes).is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
        return None;
    }
    Some(())
}

/// THE PERSON'S STAMPS AT THE COMMIT'S WRITE (LC-4 of ): the
/// sidecar beside an attempt's manifest (`seamless-<pid>-<nonce>.person`, swept
/// with the attempt's other files once its writer is gone) in which the
/// outgoing process names, right before its Commit's one write, how long ago a
/// person last gave each carried session input
/// ([`crate::App::write_person_stamps_at_commit`]), and which the successor
/// takes at its Commit ([`crate::App::take_carried_person_stamps`]). Written
/// then, not with the manifest at the park: a key the overlap passes through
/// to a shell after the park is in it.
#[cfg(unix)]
pub(crate) fn person_stamps_path(manifest: &std::path::Path) -> std::path::PathBuf {
    manifest.with_extension("person")
}

/// The most a person-stamps sidecar may hold ([`take_person_stamps`]): a line
/// of two integers per carried session, far under this for any real window.
#[cfg(unix)]
const MAX_PERSON_STAMPS_BYTES: u64 = 64 * 1024;

/// One carried session's row in the person-stamps sidecar
/// ([`write_person_stamps`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonStamp {
    /// A person last gave the session input this many milliseconds ago.
    Keyed(u64),
    /// No person ever did in the milliseconds the outgoing
    /// process (and those before it) served it
    /// (`human_input::HumanInputStamp::unkeyed_ms`).
    Unkeyed(u64),
}

/// Write the person-stamps sidecar at `path` ([`person_stamps_path`]): one
/// line per handed session, `<local id> <milliseconds since the last
/// gesture>` for a keyed one and `<local id> -<milliseconds served unkeyed>`
/// for one nobody keyed ([`PersonStamp`]), created afresh with owner-only
/// permissions. An older successor reads a `-` row as malformed and drops
/// it: that session reads never keyed there, as before. Whether it was
/// written.
#[cfg(unix)]
pub(crate) fn write_person_stamps(path: &std::path::Path, stamps: &[(u64, PersonStamp)]) -> bool {
    let body: String = stamps
        .iter()
        .map(|(local_id, stamp)| match stamp {
            PersonStamp::Keyed(age_ms) => format!("{local_id} {age_ms}\n"),
            PersonStamp::Unkeyed(age_ms) => format!("{local_id} -{age_ms}\n"),
        })
        .collect();
    write_private_new(path, body.as_bytes()).is_some()
}

/// Take the person-stamps sidecar at `path` under the private dir `dir`
/// ([`write_person_stamps`]): its `(local id, stamp)` rows, a malformed row
/// dropped; none for a missing, oversized or unreadable file — every adopted
/// session then reads as never keyed since the Commit. Consumed whatever it
/// held.
#[cfg(unix)]
pub(crate) fn take_person_stamps(
    path: &std::path::Path,
    dir: &std::path::Path,
) -> Vec<(u64, PersonStamp)> {
    let Some(body) = take_regular_capped(path, dir, MAX_PERSON_STAMPS_BYTES)
        .and_then(|bytes| String::from_utf8(bytes).ok())
    else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            let (local_id, age) = line.split_once(' ')?;
            let stamp = match age.strip_prefix('-') {
                Some(unkeyed) => PersonStamp::Unkeyed(unkeyed.parse().ok()?),
                None => PersonStamp::Keyed(age.parse().ok()?),
            };
            Some((local_id.parse().ok()?, stamp))
        })
        .collect()
}

/// Read a regular file without following a symlink, without blocking on
/// whatever else sits at the path, and without allocating past its protocol
/// cap. The caller supplies an exact private-dir path; it is consumed on every
/// parse outcome so an invalid attempt cannot be replayed.
pub(crate) fn take_regular_capped(
    path: &std::path::Path,
    dir: &std::path::Path,
    max_bytes: u64,
) -> Option<Vec<u8>> {
    if !path.starts_with(dir) {
        return None;
    }
    let result = read_regular_capped(path, max_bytes);
    let _ = std::fs::remove_file(path);
    result
}

/// [`take_regular_capped`]'s read without the consume. `handoff_diet::seal_layout` verifies a
/// layout sidecar written before the park with exactly this reader, so what it finds equal is
/// what the successor will find.
pub(crate) fn read_regular_capped(path: &std::path::Path, max_bytes: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // O_NONBLOCK: a FIFO swapped in at the path would block `open(2)` until a writer came,
        // past the parent's ready deadline. Non-blocking, it opens at once and is refused below
        // as not a regular file; a regular file is unaffected.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return None;
    }
    let capacity = usize::try_from(metadata.len()).ok()?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).ok()?;
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).ok()? <= max_bytes).then_some(bytes)
}

fn take_grid_capped(
    path: &std::path::Path,
    dir: &std::path::Path,
    per_grid_cap: u64,
    remaining: &mut u64,
) -> Option<Vec<u8>> {
    let bytes = take_regular_capped(path, dir, per_grid_cap.min(*remaining))?;
    *remaining = remaining.checked_sub(u64::try_from(bytes.len()).ok()?)?;
    Some(bytes)
}

/// Read, delete and decode one control-carry sidecar at its exact `path`. `Err(cause)` (the
/// session adopts without it; the caller logs the cause) when the stamp is malformed or names more
/// than a sidecar may hold or the handoff has left, or the file is missing, bigger than stamped,
/// truncated, of another sha, or not a sidecar this build reads. The file is deleted on every
/// outcome that reaches it.
fn take_control_sidecar(
    path: &std::path::Path,
    dir: &std::path::Path,
    stamp: &str,
    remaining: &mut u64,
) -> Result<crate::handoff_carry::ControlCarry, &'static str> {
    let Some(len) = crate::handoff_carry::stamp_len(stamp) else {
        let _ = std::fs::remove_file(path);
        return Err("its manifest stamp is malformed");
    };
    if len > crate::handoff_carry::MAX_SIDECAR_BYTES {
        let _ = std::fs::remove_file(path);
        return Err("its stamp names more than one sidecar may hold");
    }
    if len > *remaining {
        let _ = std::fs::remove_file(path);
        return Err("the handoff's control-carry budget was spent");
    }
    let bytes = take_regular_capped(path, dir, len)
        .ok_or("its sidecar is missing, not a regular file, or bigger than stamped")?;
    *remaining = remaining.saturating_sub(bytes.len() as u64);
    if !crate::handoff_carry::stamp_matches(stamp, &bytes) {
        return Err("its sidecar is not the bytes the manifest stamped (length or sha)");
    }
    crate::handoff_carry::decode_why(&bytes)
}

/// The relation a meta value had to satisfy against its bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundRule {
    /// `value < limit`.
    Below,
    /// `value <= limit`.
    AtMost,
    /// `value >= limit`.
    AtLeast,
}

impl std::fmt::Display for BoundRule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Below => "must be below",
            Self::AtMost => "must be at most",
            Self::AtLeast => "must be at least",
        })
    }
}

/// The first bound a visible-checkpoint meta broke, named field by field: which value, which
/// bound, by how much. Diagnostic only, like the [`ScreenDigestRefusal`] that carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MetaBoundViolation {
    /// The meta member the value lives in (`cursor`, `alt_cursor`,
    /// `saved_cursor_main`, `saved_cursor_alt`), or `""` for a top-level field.
    pub(crate) slot: &'static str,
    /// The field, in the meta's own spelling.
    pub(crate) field: &'static str,
    pub(crate) value: u64,
    pub(crate) rule: BoundRule,
    /// The bound's own name (`rows`, `cols`, `scroll_bottom`, `MAX_GRID_COLS`, …),
    /// or `""` for a literal bound, which prints as the number alone.
    pub(crate) limit_name: &'static str,
    pub(crate) limit: u64,
}

impl std::fmt::Display for MetaBoundViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            slot,
            field,
            value,
            rule,
            limit_name,
            limit,
        } = *self;
        if !slot.is_empty() {
            write!(formatter, "{slot}.")?;
        }
        if limit_name.is_empty() {
            write!(formatter, "{field}={value} {rule} {limit}")
        } else {
            write!(formatter, "{field}={value} {rule} {limit_name}={limit}")
        }
    }
}

/// The first bound a per-grid cursor projection breaks, if any, in the order
/// the wire's consumer checks them. `slot` names the meta member for the
/// report (`cursor` / `alt_cursor`).
fn grid_cursor_bound_violation(
    cursor: &aterm_core::terminal::GridCursorRepr,
    slot: &'static str,
    rows: u16,
    cols: u16,
) -> Option<MetaBoundViolation> {
    let broke = |field, value: u64, rule, limit_name, limit: u64| MetaBoundViolation {
        slot,
        field,
        value,
        rule,
        limit_name,
        limit,
    };
    let rows_limit = u64::from(rows);
    let cols_limit = u64::from(cols);
    let tab_stops = u64::try_from(cursor.tab_stops.len()).unwrap_or(u64::MAX);
    if cursor.cursor_row >= rows {
        return Some(broke(
            "cursor_row",
            u64::from(cursor.cursor_row),
            BoundRule::Below,
            "rows",
            rows_limit,
        ));
    }
    if cursor.cursor_col >= cols {
        return Some(broke(
            "cursor_col",
            u64::from(cursor.cursor_col),
            BoundRule::Below,
            "cols",
            cols_limit,
        ));
    }
    if cursor.scroll_top > cursor.scroll_bottom {
        return Some(broke(
            "scroll_top",
            u64::from(cursor.scroll_top),
            BoundRule::AtMost,
            "scroll_bottom",
            u64::from(cursor.scroll_bottom),
        ));
    }
    if cursor.scroll_bottom >= rows {
        return Some(broke(
            "scroll_bottom",
            u64::from(cursor.scroll_bottom),
            BoundRule::Below,
            "rows",
            rows_limit,
        ));
    }
    if cursor.margin_left > cursor.margin_right {
        return Some(broke(
            "margin_left",
            u64::from(cursor.margin_left),
            BoundRule::AtMost,
            "margin_right",
            u64::from(cursor.margin_right),
        ));
    }
    if cursor.margin_right >= cols {
        return Some(broke(
            "margin_right",
            u64::from(cursor.margin_right),
            BoundRule::Below,
            "cols",
            cols_limit,
        ));
    }
    // Grid resize deliberately never shrinks the stop vector: stops beyond
    // the current width must survive a later grow. Require coverage of every
    // active column, while independently capping the carried allocation at
    // the engine's protocol maximum.
    if tab_stops < cols_limit {
        return Some(broke(
            "tab_stops.len()",
            tab_stops,
            BoundRule::AtLeast,
            "cols",
            cols_limit,
        ));
    }
    if tab_stops > u64::from(aterm_core::grid::MAX_GRID_COLS) {
        return Some(broke(
            "tab_stops.len()",
            tab_stops,
            BoundRule::AtMost,
            "MAX_GRID_COLS",
            u64::from(aterm_core::grid::MAX_GRID_COLS),
        ));
    }
    None
}

/// The first bound `meta` breaks, in the order the wire's consumer checks
/// them — so the field a refusal names is the one the successor would have
/// refused on first. `None` is the whole predicate holding.
fn checkpoint_meta_bound_violation(meta: &CheckpointMeta) -> Option<MetaBoundViolation> {
    let rows = meta.rows;
    let cols = meta.cols;
    let top = |field, value: u64, rule, limit_name, limit: u64| MetaBoundViolation {
        slot: "",
        field,
        value,
        rule,
        limit_name,
        limit,
    };
    if rows == 0 {
        return Some(top("rows", 0, BoundRule::AtLeast, "", 1));
    }
    if cols == 0 {
        return Some(top("cols", 0, BoundRule::AtLeast, "", 1));
    }
    if rows > aterm_core::grid::MAX_GRID_ROWS {
        return Some(top(
            "rows",
            u64::from(rows),
            BoundRule::AtMost,
            "MAX_GRID_ROWS",
            u64::from(aterm_core::grid::MAX_GRID_ROWS),
        ));
    }
    if cols > aterm_core::grid::MAX_GRID_COLS {
        return Some(top(
            "cols",
            u64::from(cols),
            BoundRule::AtMost,
            "MAX_GRID_COLS",
            u64::from(aterm_core::grid::MAX_GRID_COLS),
        ));
    }
    if let Some(violation) = grid_cursor_bound_violation(&meta.cursor, "cursor", rows, cols) {
        return Some(violation);
    }
    if let Some(violation) = meta
        .alt_cursor
        .as_ref()
        .and_then(|cursor| grid_cursor_bound_violation(cursor, "alt_cursor", rows, cols))
    {
        return Some(violation);
    }
    // The DECSC slots are bounded by the protocol, not by the grid. A saved cursor is absolute
    // coordinates nothing reads until DECRC or a 1049 exit, and those clamp to the grid at that
    // moment (`Grid::set_cursor`; xterm's `CursorRestore` likewise). `Terminal::resize` moves a
    // slot only with its text (xterm's `AdjustSavedCursor`, `Grid::lend_saved_cursor`), so after
    // a shrink that trimmed the blank row it was on, an honest slot names a row the grid no longer
    // has; if the grid grows back first, the cursor lands on that row, which a restored engine
    // must keep. Bounding the slot at `rows` refuses such honest carries (a one-row chrome bar
    // shrinks every session by a row) and clamping at capture lands a later 1049 exit a row off
    // its text, so the bound is the engine's own ceiling. The restore path indexes nothing with
    // it (`SavedCursorRepr::into_saved` builds a bare `Cursor`): a sanity bound, not a memory one.
    let max_rows = aterm_core::grid::MAX_GRID_ROWS;
    let max_cols = aterm_core::grid::MAX_GRID_COLS;
    for (slot, saved) in [
        ("saved_cursor_main", meta.saved_cursor_main),
        ("saved_cursor_alt", meta.saved_cursor_alt),
    ] {
        let Some(saved) = saved else {
            continue;
        };
        if saved.cursor_row >= max_rows {
            return Some(MetaBoundViolation {
                slot,
                field: "cursor_row",
                value: u64::from(saved.cursor_row),
                rule: BoundRule::Below,
                limit_name: "MAX_GRID_ROWS",
                limit: u64::from(max_rows),
            });
        }
        if saved.cursor_col >= max_cols {
            return Some(MetaBoundViolation {
                slot,
                field: "cursor_col",
                value: u64::from(saved.cursor_col),
                rule: BoundRule::Below,
                limit_name: "MAX_GRID_COLS",
                limit: u64::from(max_cols),
            });
        }
    }
    if let Some(cwd) = meta.current_working_directory.as_ref() {
        let len = u64::try_from(cwd.len()).unwrap_or(u64::MAX);
        if len > 8 * 1024 {
            return Some(top(
                "current_working_directory.len()",
                len,
                BoundRule::AtMost,
                "8 KiB",
                8 * 1024,
            ));
        }
        let nul_bytes =
            u64::try_from(cwd.bytes().filter(|byte| *byte == 0).count()).unwrap_or(u64::MAX);
        if nul_bytes > 0 {
            return Some(top(
                "current_working_directory NUL bytes",
                nul_bytes,
                BoundRule::AtMost,
                "",
                0,
            ));
        }
    }
    None
}

/// `true` when every named bound holds — [`checkpoint_meta_bound_violation`]
/// is `None`. The predicate's own spelling, kept for the cap and the tests.
#[cfg(any(unix, test))]
fn checkpoint_meta_is_bounded(meta: &CheckpointMeta) -> bool {
    checkpoint_meta_bound_violation(meta).is_none()
}

// The `CheckpointMeta` keys a modern screen carry must name before this build
// deserializes it: aterm-core's FROZEN list (it may only shrink; its doc says why),
// shared with the crash-restore journal's keyframe reader so the two doors refuse the
// same metas. `the_required_meta_keys_are_frozen` pins it verbatim.
use aterm_core::terminal::CHECKPOINT_META_REQUIRED_KEYS;

fn parse_checkpoint_meta(carry: &ScreenCarry) -> Option<CheckpointMeta> {
    if carry.schema != ScreenCarry::SCHEMA {
        return None;
    }
    let value: aterm_json::Value = aterm_json::from_str(&carry.meta).ok()?;
    let object = value.as_object()?;
    if !CHECKPOINT_META_REQUIRED_KEYS
        .iter()
        .all(|key| object.contains_key(*key))
    {
        return None;
    }
    aterm_json::from_value(value).ok()
}

fn dimension_grid_cap(rows: u16, cols: u16, history: u32) -> Option<u64> {
    if rows == 0
        || cols == 0
        || rows > aterm_core::grid::MAX_GRID_ROWS
        || cols > aterm_core::grid::MAX_GRID_COLS
        || history > MAX_HANDOFF_HISTORY_LINES
    {
        return None;
    }
    // A carried checkpoint holds `history + rows` records, so the budget must be
    // priced over all of them. `history` is already bounded above, so this cannot
    // be inflated by a hostile meta.
    let rows = u64::from(rows).checked_add(u64::from(history))?;
    // The cap scales with cells plus bounded per-line framing/attribute overhead, under a
    // protocol-wide ceiling so a maximum-dimension hostile meta never authorizes a
    // multi-gigabyte startup allocation.
    let cells = rows.checked_mul(u64::from(cols))?;
    let cell_budget = cells.checked_mul(512)?;
    let line_budget = rows.checked_mul(16 * 1024)?;
    Some(
        64_u64
            .checked_mul(1024)?
            .checked_add(cell_budget)?
            .checked_add(line_budget)?
            .min(MAX_HANDOFF_GRID_BYTES),
    )
}

#[cfg(any(unix, test))]
fn checkpoint_grid_cap(meta: &CheckpointMeta) -> Option<u64> {
    checkpoint_meta_is_bounded(meta).then_some(())?;
    dimension_grid_cap(meta.rows, meta.cols, meta.history_lines)
}

/// Which ceiling [`admit_checkpoint_dimensions`] refused a geometry on, so a refusal names the cap
/// that actually bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmitRefusal {
    /// The geometry or history is outside the protocol itself: a zero side,
    /// a side past the engine's `MAX_GRID_ROWS`/`MAX_GRID_COLS`, or more than
    /// `MAX_HANDOFF_HISTORY_LINES` carried lines.
    Dimension { rows: u16, cols: u16, history: u32 },
    /// One grid's `rows * cols` is over the per-grid cell ceiling in force.
    PerGrid { cells: u64, cap: u64 },
    /// This session's cost would take the handoff's decoded cells past the
    /// aggregate.
    Aggregate { cost: u64, used: u64, cap: u64 },
}

impl std::fmt::Display for AdmitRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Dimension {
                rows,
                cols,
                history,
            } => write!(
                formatter,
                "{rows}x{cols} with {history} carried line(s) is outside the protocol's \
                 dimensions"
            ),
            Self::PerGrid { cells, cap } => write!(
                formatter,
                "{cells} cells is over the {cap}-cell per-grid cap"
            ),
            Self::Aggregate { cost, used, cap } => write!(
                formatter,
                "{cost} more cells on top of {used} is over the {cap}-cell aggregate grid-cell \
                 budget"
            ),
        }
    }
}

/// The consumer's ceilings a producer must fit.
///
/// Consumer checks only ever become more lenient (law L3), so a successor
/// NEWER than this build admits at least what this build admits and gets
/// [`Self::current`]. A ROLLBACK is the one hop that runs the other way: its
/// successor is an older build whose checks cannot be relaxed from here. One
/// from [`RELAXED_CONSUMER_FROM_BUILD`] on enforces this build's ceilings; one
/// before it gets [`Self::legacy`], and a session one of those stricter checks
/// would refuse is carried at a lower rung ([`carry_for_wire`]) instead of
/// being sent a grid its consumer refuses — which a consumer of v0.91.0's
/// vintage answers by refusing the WHOLE adoption.
///
/// The consumer checks this branch relaxed, and so the whole of what a
/// legacy consumer is stricter about (audited against `v0.91.0` on 2026-09-24):
///
/// It may also predate the parser and output-state carries. The aggregate, the byte caps, the
/// meta bounds and the required meta keys are the same in every consumer from v0.91.0 on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireCaps {
    per_grid_cells: u64,
    /// The successor's strict line decoder bounds a line's attrs RLE by the
    /// grid's column count alone — v0.92.0 and every consumer before it.
    attrs_bounded_by_columns: bool,
    /// The successor restores the parser's partial state (`CheckpointMeta.parser`). A consumer
    /// that predates it ignores the key and resumes at Ground, abandoning any partial sequence it
    /// is handed ([`Self::carries_parser`]).
    carries_parser: bool,
    /// The successor restores the engine's in-flight output state (`CheckpointMeta.output_state`:
    /// the REP character, an SGR 58 underline colour, an open OSC 8 link, a VT52 address in
    /// progress, a pending ZWJ, the XTPUSHSGR stack, the XTSAVE'd modes). A consumer that predates
    /// it ignores the key and resumes with none of them ([`Self::carries_output_state`]).
    carries_output_state: bool,
}

impl WireCaps {
    /// This build's own ceilings — what its consumer and `screen_digest`
    /// enforce.
    #[must_use]
    pub(crate) const fn current() -> Self {
        Self {
            per_grid_cells: MAX_HANDOFF_GRID_CELLS,
            attrs_bounded_by_columns: false,
            carries_parser: true,
            carries_output_state: true,
        }
    }

    /// The frozen ceilings of every consumer before
    /// [`RELAXED_CONSUMER_FROM_BUILD`] (audited against v0.91.0; v0.92.0
    /// enforces the same).
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) const fn legacy() -> Self {
        Self {
            per_grid_cells: LEGACY_HANDOFF_GRID_CELLS,
            attrs_bounded_by_columns: true,
            // Conservative: an older successor may predate the parser carry,
            // and nothing on the wire says whether it does.
            carries_parser: false,
            // The same: every build before this one predates the output-state
            // carry.
            carries_output_state: false,
        }
    }

    /// The ceilings of the successor at `target_build`, handed to by the
    /// running build.
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) fn for_target(target_build: u64) -> Self {
        Self::for_hop(crate::running_build_number(), target_build)
    }

    /// [`Self::for_target`] with the running build named, so a test can pin
    /// both directions of a hop. An older successor from
    /// [`RELAXED_CONSUMER_FROM_BUILD`] on admits this build's ceilings, but
    /// what it restores beyond the screen is not known from here, so it is
    /// handed no parser or output state (round eight's lead U24: such a hop
    /// was priced as v0.91.0's, and a pane past 32 Ki cells crossed blank).
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) fn for_hop(running_build: u64, target_build: u64) -> Self {
        if target_build >= running_build {
            Self::current()
        } else if target_build >= RELAXED_CONSUMER_FROM_BUILD {
            Self {
                carries_parser: false,
                carries_output_state: false,
                ..Self::current()
            }
        } else {
            Self::legacy()
        }
    }

    /// Whether the successor continues a partial escape sequence the carry
    /// holds. `false` for every hop to an OLDER build ([`Self::for_hop`]), some
    /// of which predate the parser carry and would resume at Ground — so the
    /// capture treats ANY partial sequence over queued output as a timing miss
    /// there (`Terminal::partial_sequence_state`), not only the hooked DCS no
    /// carry holds (`Terminal::partial_sequence_uncarried`): the queued tail of
    /// a split CSI would otherwise print as text in that successor.
    #[cfg(unix)]
    #[must_use]
    pub(crate) const fn carries_parser(self) -> bool {
        self.carries_parser
    }

    /// Whether the successor resumes with the engine's in-flight output state
    /// the carry holds (`CheckpointMeta.output_state`). `false` for every hop
    /// to an OLDER build ([`Self::for_hop`]), some of which predate it — so
    /// there the capture treats an open OSC 8 link or a pending VT52
    /// address over queued output as a timing miss
    /// (`Terminal::output_state_in_flight`): that successor would write the
    /// rest of the link's text unlinked and print the address bytes. To a
    /// successor that carries it, neither is a hazard.
    #[cfg(unix)]
    #[must_use]
    pub(crate) const fn carries_output_state(self) -> bool {
        self.carries_output_state
    }

    /// The per-grid cell ceiling in force.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn per_grid_cells(self) -> u64 {
        self.per_grid_cells
    }

    /// [`admit_checkpoint_dimensions`] against these ceilings: the producer's seam, which must
    /// price every carry at the target's cap.
    pub(crate) fn admit(
        self,
        used_cells: &mut u64,
        rows: u16,
        cols: u16,
        history: u32,
        has_alt: bool,
    ) -> Result<u64, AdmitRefusal> {
        let dimension = AdmitRefusal::Dimension {
            rows,
            cols,
            history,
        };
        let cap = dimension_grid_cap(rows, cols, history).ok_or(dimension)?;
        // The visible grid keeps a per-grid ceiling: that bound refuses an absurd screen
        // geometry and has nothing to do with history.
        let cells = u64::from(rows)
            .checked_mul(u64::from(cols))
            .ok_or(dimension)?;
        if cells > self.per_grid_cells {
            return Err(AdmitRefusal::PerGrid {
                cells,
                cap: self.per_grid_cells,
            });
        }
        // Carried history is priced on top, bounded separately by
        // MAX_HANDOFF_HISTORY_LINES (already enforced in `dimension_grid_cap`).
        let history_cells = u64::from(history)
            .checked_mul(u64::from(cols))
            .ok_or(dimension)?;
        // The alt grid never carries history (the live alt screen keeps no
        // scrollback), so it costs one visible grid, not one carried grid.
        let cost = cells
            .checked_add(history_cells)
            .and_then(|cost| cost.checked_add(if has_alt { cells } else { 0 }))
            .ok_or(dimension)?;
        let aggregate = AdmitRefusal::Aggregate {
            cost,
            used: *used_cells,
            cap: MAX_HANDOFF_AGGREGATE_GRID_CELLS,
        };
        let next = used_cells.checked_add(cost).ok_or(aggregate)?;
        if next > MAX_HANDOFF_AGGREGATE_GRID_CELLS {
            return Err(aggregate);
        }
        *used_cells = next;
        Ok(cap)
    }

    /// The first line of `checkpoint`'s grids that this successor's strict line decoder refuses
    /// although this build's admits it, named; `None` when the successor decodes every line this
    /// build does (always, for a successor at least as new). A v0.91.0-era consumer bounds a
    /// line's attrs RLE by the grid's columns, where this build uses its character count, so it
    /// finds a full-width styled row holding `e` + U+0301 or a ZWJ cluster non-canonical and
    /// refuses the whole adoption; on a rollback hop such a carry goes down the ladder. The
    /// arithmetic is v0.91.0's `decode_line_strict`: attrs run lengths summed, against the columns.
    #[cfg(any(unix, test))]
    fn line_decoder_refusal(self, checkpoint: &TerminalCheckpoint) -> Option<String> {
        if !self.attrs_bounded_by_columns {
            return None;
        }
        let cols = u64::from(checkpoint.cols);
        [
            (
                GridSlot::Main,
                Some(&checkpoint.grid),
                checkpoint.history_lines,
            ),
            (GridSlot::Inactive, checkpoint.alt_grid.as_ref(), 0),
        ]
        .into_iter()
        .find_map(|(which, blob, history)| {
            // A blob this build cannot decode is the shape refusal's to name,
            // and it runs first; there is nothing more to say about it here.
            let lines = strict_grid_lines(blob?, checkpoint.rows, checkpoint.cols, history)?;
            lines.iter().enumerate().find_map(|(record, line)| {
                let entries = line.attrs().map_or(0_u64, |attrs| {
                    attrs
                        .runs()
                        .iter()
                        .map(|run| u64::from(run.length))
                        .fold(0_u64, u64::saturating_add)
                });
                (entries > cols).then(|| {
                    format!(
                        "{which} record {record} carries {entries} styled characters over its \
                         {cols} columns, which the older successor's line decoder refuses"
                    )
                })
            })
        })
    }
}

/// One dimension/allocation admission seam shared by UI pre-capture,
/// outgoing digest/write, and incoming decode, at this build's own ceilings
/// ([`WireCaps::current`]). The aggregate changes only on success, so an
/// over-budget checkpoint cannot leave partial authority; a refusal names the
/// ceiling that bound.
pub(crate) fn admit_checkpoint_dimensions(
    used_cells: &mut u64,
    rows: u16,
    cols: u16,
    history: u32,
    has_alt: bool,
) -> Result<u64, AdmitRefusal> {
    WireCaps::current().admit(used_cells, rows, cols, history, has_alt)
}

/// [`admit_checkpoint_dimensions`] for a crash restore's checkpoint (`crash_restore`). Its
/// history is not the handoff wire's (at most `MAX_HANDOFF_HISTORY_LINES`): it is the
/// keyframe's bridge (at most `aterm_restore::policy::BRIDGE_CAP` lines) plus the lines the
/// tail's replay pushed (bounded by the tail ring, 256 KiB), already decoded in the
/// restorer's memory. So the visible grid and its alternate screen are priced exactly as a
/// handoff's, and every history line on top of them against the same aggregate: a
/// recovery whose screens would together pass it takes the META rung for the rest. The
/// aggregate changes only on success.
#[cfg(unix)]
pub(crate) fn admit_restored_dimensions(
    used_cells: &mut u64,
    rows: u16,
    cols: u16,
    history: u32,
    has_alt: bool,
) -> Result<(), AdmitRefusal> {
    let mut charged = *used_cells;
    WireCaps::current().admit(&mut charged, rows, cols, 0, has_alt)?;
    let history_cells = u64::from(history) * u64::from(cols);
    let aggregate = AdmitRefusal::Aggregate {
        cost: history_cells,
        used: charged,
        cap: MAX_HANDOFF_AGGREGATE_GRID_CELLS,
    };
    let next = charged.checked_add(history_cells).ok_or(aggregate)?;
    if next > MAX_HANDOFF_AGGREGATE_GRID_CELLS {
        return Err(aggregate);
    }
    *used_cells = next;
    Ok(())
}

/// What one session's mandatory carry costs the aggregate: its visible grid plus the alt grid the
/// producer conservatively reserves, exactly what
/// `admit_checkpoint_dimensions(used, rows, cols, 0, true)` charges. Separate because the producer
/// prices a whole pool before charging any of it, which the per-session admission cannot answer.
#[must_use]
#[cfg(unix)]
pub(crate) fn mandatory_checkpoint_cells(rows: u16, cols: u16) -> u64 {
    u64::from(rows)
        .saturating_mul(u64::from(cols))
        .saturating_mul(2)
}

/// The same pricing for the producer's capture-byte budget: twice `dimension_grid_cap`, what the
/// capture loop charges per admitted session (the alt grid is reserved conservatively, as at
/// pre-capture admission). `None` for a geometry `dimension_grid_cap` refuses.
#[must_use]
#[cfg(any(unix, test))]
pub(crate) fn checkpoint_capture_budget_bytes(rows: u16, cols: u16, history: u32) -> Option<u64> {
    dimension_grid_cap(rows, cols, history)?.checked_mul(2)
}

/// The process-wide aggregate cell ceiling, so the producer prices a whole pool against the number
/// `admit_checkpoint_dimensions` enforces.
#[must_use]
#[cfg(unix)]
pub(crate) fn max_handoff_aggregate_grid_cells() -> u64 {
    MAX_HANDOFF_AGGREGATE_GRID_CELLS
}

/// The byte ceiling on one serialized line record of a carried grid at `cols`: 16 KiB of framing
/// plus 512 bytes per column. Every consumer since the line codec went on the wire enforces it, so
/// the producer must honour it and may never raise it. The link-stripping rung
/// ([`strip_over_cap_links`]) asks the strict decoder's question with this same arithmetic.
fn line_record_cap(cols: u16) -> usize {
    16usize
        .saturating_mul(1024)
        .saturating_add(usize::from(cols).saturating_mul(512))
}

/// The strict decode of one grid blob that must hold exactly `history + rows`
/// records at `cols`: the bounded half of [`checkpoint_grid_is_canonical`],
/// shared with the producer's history drop ([`drop_carried_history`]) so both
/// read a blob under the same bounds.
fn strict_grid_lines(
    bytes: &[u8],
    rows: u16,
    cols: u16,
    history: u32,
) -> Option<Vec<aterm_core::scrollback::Line>> {
    // A materialized grid cell holds at most one 256-byte grapheme unit. Bound
    // content and full record framing from the authenticated column count before
    // the decoder allocates any line payload or sidecars.
    let content_cap = usize::from(cols).saturating_mul(256);
    let record_cap = line_record_cap(cols);
    // A carried checkpoint is `history` scrollback records followed by exactly
    // `rows` visible records. `history` is bounded by the caller's meta check, so
    // this total can never be inflated by the payload itself.
    if history > MAX_HANDOFF_HISTORY_LINES {
        return None;
    }
    let expected = usize::from(rows).checked_add(history as usize)?;
    let lines = aterm_core::scrollback::deserialize_lines_strict(
        bytes,
        expected,
        usize::from(cols),
        content_cap,
        record_cap,
    )?;
    (lines.len() == expected).then_some(lines)
}

fn checkpoint_grid_is_canonical(bytes: &[u8], rows: u16, cols: u16, history: u32) -> bool {
    strict_grid_lines(bytes, rows, cols, history)
        .is_some_and(|lines| aterm_core::scrollback::serialize_lines(&lines).as_slice() == bytes)
}

/// The raw bytes one session's screen carry put on the wire, read under the resource caps only:
/// what the adoption proof hashes, whatever this build then makes of them.
struct ScreenWireBytes {
    grid: Vec<u8>,
    alt_grid: Option<Vec<u8>>,
}

/// What [`take_incoming_from`] made of one session's carried screen.
enum IncomingScreen {
    /// The carried screen, exactly: the checkpoint's grid blobs are the wire bytes, validated
    /// canonical and moved in unchanged.
    Exact(TerminalCheckpoint),
    /// This build refused the screen's content, so the session adopts onto `checkpoint` (a blank
    /// canonical screen at a sanitized meta, [`repaint_checkpoint`], or `None` for a blank engine
    /// at the pane's own geometry when not even the meta parsed) and is repainted. `wire` keeps
    /// the bytes read so the proof still covers them.
    Degraded {
        checkpoint: Option<TerminalCheckpoint>,
        cause: String,
        wire: ScreenWireBytes,
    },
}

/// One session [`take_incoming_from`] adopts, with why its screen was degraded and
/// the raw bytes a degraded one keeps for the proof (an exact one's live in its
/// checkpoint already).
struct IncomingSession {
    adopted: Adopted,
    degraded: Option<(String, ScreenWireBytes)>,
}

/// Log why one session's hard check refuses the whole handoff, and refuse it: a successor launched
/// through LaunchServices has no stderr, and the parent sees only its readiness pipe close
/// (`ChildDied`). Only identity and resource facts come here; a screen's content degrades instead
/// ([`admit_incoming_screen`]).
fn refuse_incoming_session<T>(local_id: u64, why: &str) -> Option<T> {
    aterm_log::warn!(
        "overlap handoff: session {local_id} {why}; refusing the whole handoff so the outgoing \
         process keeps every session"
    );
    None
}

/// Read one session's grid sidecars at their exact paths, priced by resource alone: the hard half
/// of the consumer's screen check. `Err` refuses the whole adoption: a carry naming a sidecar
/// other than its own is not this attempt's, and a sidecar over its byte cap or past the
/// aggregate is an allocation the successor must not make.
///
/// The cap is [`dimension_grid_cap`], not [`checkpoint_grid_cap`]: the latter is `None` whenever
/// the meta breaks a semantic bound, which would leave a content refusal nothing to price the read
/// with. The dimension cap depends on geometry and history alone and falls back to
/// `MAX_HANDOFF_GRID_BYTES` when the meta did not parse or names a geometry the protocol lacks;
/// every byte still counts against the aggregate. Within every meta bound the two caps agree.
fn take_screen_wire(
    carry: &ScreenCarry,
    meta: Option<&CheckpointMeta>,
    grid_path: &std::path::Path,
    alt_path: &std::path::Path,
    dir: &std::path::Path,
    remaining: &mut u64,
) -> Result<ScreenWireBytes, &'static str> {
    if std::path::Path::new(&carry.grid_file) != grid_path
        || carry
            .alt_grid_file
            .as_deref()
            .is_some_and(|encoded| std::path::Path::new(encoded) != alt_path)
    {
        return Err("names a grid sidecar other than its own");
    }
    let cap = meta
        .and_then(|meta| dimension_grid_cap(meta.rows, meta.cols, meta.history_lines))
        .unwrap_or(MAX_HANDOFF_GRID_BYTES);
    let grid = take_grid_capped(grid_path, dir, cap, remaining).ok_or(
        "has a main grid sidecar that is missing, over its byte cap, or past the handoff's \
         aggregate byte cap",
    )?;
    let alt_grid = match carry.alt_grid_file {
        Some(_) => Some(take_grid_capped(alt_path, dir, cap, remaining).ok_or(
            "has an inactive grid sidecar that is missing, over its byte cap, or past the \
             handoff's aggregate byte cap",
        )?),
        None => None,
    };
    Ok(ScreenWireBytes { grid, alt_grid })
}

/// Judge one session's carried screen with this build's content predicates: the half of the
/// consumer's check that may cost a screen but never the update. The consumer is always the newer
/// build, the only side whose rules a release can fix, so a refused screen degrades that one
/// session to a blank, repainted one. An admitted screen is charged to the aggregate cell budget,
/// dimensions before a byte is decoded.
fn admit_incoming_screen(
    meta: Option<CheckpointMeta>,
    wire: ScreenWireBytes,
    used_cells: &mut u64,
) -> IncomingScreen {
    // A readable meta always yields a checkpoint, so the shell's mark authority
    // crosses with it: past a spent aggregate, the 1x1 last resort, charged to
    // nothing (one cell is not what the budget bounds). Only an unreadable meta
    // adopts with none, and `spawn::hold_adopted_shell_nonce_requirement` keeps
    // the requirement standing there.
    let degrade = |meta: Option<&CheckpointMeta>, cause: String, wire, used_cells: &mut u64| {
        IncomingScreen::Degraded {
            checkpoint: repaint_checkpoint(meta, used_cells)
                .or_else(|| meta.map(unadmitted_blank_carry)),
            cause,
            wire,
        }
    };
    let Some(meta) = meta else {
        return degrade(
            None,
            "the screen meta did not parse (schema, JSON or a required key)".to_string(),
            wire,
            used_cells,
        );
    };
    if let Some(violation) = checkpoint_meta_bound_violation(&meta) {
        return degrade(
            Some(&meta),
            format!(
                "meta out of bounds at {}x{}: {violation}",
                meta.rows, meta.cols
            ),
            wire,
            used_cells,
        );
    }
    if meta.alt_cursor.is_some() != wire.alt_grid.is_some() {
        return degrade(
            Some(&meta),
            "the inactive grid's presence disagrees with the meta".to_string(),
            wire,
            used_cells,
        );
    }
    // Priced on a copy: a screen refused after this point must not keep the
    // cells an exact adoption would have used.
    let mut charged = *used_cells;
    if let Err(bound) = admit_checkpoint_dimensions(
        &mut charged,
        meta.rows,
        meta.cols,
        meta.history_lines,
        meta.alt_cursor.is_some(),
    ) {
        return degrade(
            Some(&meta),
            format!(
                "{}x{} with {} history line(s) was refused: {bound}",
                meta.rows, meta.cols, meta.history_lines
            ),
            wire,
            used_cells,
        );
    }
    if !checkpoint_grid_is_canonical(&wire.grid, meta.rows, meta.cols, meta.history_lines) {
        return degrade(
            Some(&meta),
            format!("the {} is not canonical", GridSlot::Main),
            wire,
            used_cells,
        );
    }
    // The inactive grid never carries history.
    if wire
        .alt_grid
        .as_deref()
        .is_some_and(|alt| !checkpoint_grid_is_canonical(alt, meta.rows, meta.cols, 0))
    {
        return degrade(
            Some(&meta),
            format!("the {} is not canonical", GridSlot::Inactive),
            wire,
            used_cells,
        );
    }
    *used_cells = charged;
    IncomingScreen::Exact(meta.into_checkpoint(wire.grid, wire.alt_grid))
}

/// A blank screen carrying every piece of `meta`'s scalar state this build admits: the checkpoint
/// a session is adopted onto when its real screen cannot be (the producer's Repaint rung and the
/// consumer's degrade share it).
///
/// Canonical by construction: the grids are a fresh engine's serialized blank rows, and every
/// scalar is kept if this build's bound admits it or else replaced by the fresh engine's value. A
/// cursor that breaks a bound keeps only its position, clamped (so a program that redraws
/// relative to it lands on the same row); a DECSC slot past the engine's ceiling and a directory
/// holding a NUL or over the length cap are dropped. Modes, keyboard protocols, charsets and the
/// pen survive, which keeps typing into a degraded TUI working until it redraws.
///
/// Geometry: the carried size when the aggregate cell budget admits it, else at most 24x80, else
/// 1x1; the window's first resize converges it to the pane. `None` when not even 1x1 fits or
/// there is no meta. The consumer's degrade then takes [`unadmitted_blank_carry`] for a readable
/// meta, so the shell's mark authority still crosses; with no meta the session adopts onto a blank
/// engine that `spawn_session` builds with the nonce requirement standing.
fn repaint_checkpoint(
    meta: Option<&CheckpointMeta>,
    used_cells: &mut u64,
) -> Option<TerminalCheckpoint> {
    repaint_checkpoint_within(meta, used_cells, WireCaps::current())
}

/// [`repaint_checkpoint`] priced at a successor's ceilings: the producer's Repaint rung must fit
/// the consumer it hands to, which on a rollback has a smaller per-grid cap
/// ([`WireCaps::for_target`]). A fresh engine's two grids at one geometry are the same blank rows,
/// so its serialized blank screen serves both slots beside the sanitized meta's scalar state.
fn repaint_checkpoint_within(
    meta: Option<&CheckpointMeta>,
    used_cells: &mut u64,
    caps: WireCaps,
) -> Option<TerminalCheckpoint> {
    let meta = meta?;
    // An alternate screen keeps its saved primary, so a later 1049 exit has a
    // grid to return to.
    let has_alt = meta.alt_cursor.is_some() || meta.modes.alternate_screen;
    let rows = meta.rows.clamp(1, aterm_core::grid::MAX_GRID_ROWS);
    let cols = meta.cols.clamp(1, aterm_core::grid::MAX_GRID_COLS);
    let (rows, cols) = [(rows, cols), (rows.min(24), cols.min(80)), (1, 1)]
        .into_iter()
        .find(|&(rows, cols)| caps.admit(used_cells, rows, cols, 0, has_alt).is_ok())?;
    let blank = aterm_core::terminal::Terminal::new(rows, cols).checkpoint_carry(0)?;
    let sanitized_cursor = |carried: &aterm_core::terminal::GridCursorRepr| {
        if grid_cursor_bound_violation(carried, "", rows, cols).is_none() {
            carried.clone()
        } else {
            aterm_core::terminal::GridCursorRepr {
                cursor_row: carried.cursor_row.min(rows - 1),
                cursor_col: carried.cursor_col.min(cols - 1),
                pending_wrap: false,
                ..blank.cursor.clone()
            }
        }
    };
    let mut sanitized = meta.clone();
    sanitized.rows = rows;
    sanitized.cols = cols;
    sanitized.history_lines = 0;
    sanitized.cursor = sanitized_cursor(&meta.cursor);
    sanitized.alt_cursor =
        has_alt.then(|| sanitized_cursor(meta.alt_cursor.as_ref().unwrap_or(&blank.cursor)));
    // The screen is blank: the rows the carried resizes moved are not on it, so
    // nothing is owed back (a fresh engine's carry owes nothing).
    for cursor in std::iter::once(&mut sanitized.cursor).chain(sanitized.alt_cursor.as_mut()) {
        cursor.handback_rows = 0;
        cursor.handback_above = 0;
    }
    for slot in [
        &mut sanitized.saved_cursor_main,
        &mut sanitized.saved_cursor_alt,
    ] {
        *slot = slot.filter(|saved| {
            saved.cursor_row < aterm_core::grid::MAX_GRID_ROWS
                && saved.cursor_col < aterm_core::grid::MAX_GRID_COLS
        });
    }
    sanitized.current_working_directory = meta
        .current_working_directory
        .clone()
        .filter(|cwd| cwd.len() <= 8 * 1024 && !cwd.contains('\0'));
    // Belt and braces for a bound added later and not sanitized above: the
    // fresh engine's own projection holds every bound this build has — and the
    // shell's mark authority, which is no bound's business.
    if checkpoint_meta_bound_violation(&sanitized).is_some() {
        return Some(keep_carried_title(
            keep_shell_integration_authority(blank, meta),
            meta,
        ));
    }
    let alt_grid = has_alt.then(|| blank.grid.clone());
    Some(sanitized.into_checkpoint(blank.grid, alt_grid))
}

/// Where on the producer's degrade ladder one session's screen was carried. The checks choose a
/// rung and never refuse the update (the outgoing build is the older one, so no release could fix
/// a veto here): every rung's carry passes this build's own predicates, so the producer never
/// sends what its own consumer would reject, and the worst case is one blank tab that redraws.
/// Ranked by the loss each names, least first; a carry with two losses (links stripped, then
/// scrollback dropped by the self-check) is labelled with the larger, hence `max`. The ladder
/// tries them in its own order ([`carry_for_wire`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg(any(unix, test))]
pub(crate) enum CarryRung {
    /// The screen as the capture asked for it: the visible grid, the saved
    /// primary, and the scrollback depth the time and budget ladder chose.
    Full,
    /// A predicate refused the carried scrollback, so the screen went without
    /// it: the visible grid and every scalar exactly.
    VisibleOnly,
    /// A line record past the wire's per-record byte cap refused the exact screen, so it went with
    /// the hyperlinks of exactly those lines dropped ([`strip_over_cap_links`]): every character,
    /// attribute, underline colour, wrap flag and scalar is exact, and the scrollback is kept when
    /// it fits. Only OSC 8 destinations push an honest line past that cap (a few 8 KiB URLs in one
    /// row).
    ///
    /// Ranked below [`Self::VisibleOnly`] so a stripped carry whose scrollback the self-check
    /// drops later stays labelled StrippedLinks; the ladder still tries it at each scrollback
    /// depth before giving that depth up ([`carry_for_wire`]).
    StrippedLinks,
    /// The screen's grids exactly, with the scalar fields that broke a meta
    /// bound clamped ([`sanitize_checkpoint_for_wire`]).
    Sanitized,
    /// A blank, canonical screen carrying the sanitized scalar state: modes,
    /// keyboard protocols, charsets and the pen survive, the content redraws.
    Repaint,
}

#[cfg(any(unix, test))]
impl CarryRung {
    /// The successor must make the program redraw (`ScreenCarry::repaint`): its screen is not
    /// exactly what the program last drew. Not [`Self::StrippedLinks`]: its text, attributes and
    /// cursor are exact, and a redraw (a resize bounce, for a shell) would only restore link
    /// destinations a shell never re-emits.
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) const fn needs_repaint(self) -> bool {
        matches!(self, Self::Sanitized | Self::Repaint)
    }

    /// Whether the control carry's differ state (`handoff_carry::capture_head`) travels: only
    /// while the carried screen is the one it describes, since it diffs the program's next frame
    /// against that screen. The rest of the control carry (the turn ledger, the archive's
    /// scrolled-off rows) travels at every rung; without the differ state the adopting archive
    /// starts a new baseline behind a `restore` gap. [`Self::StrippedLinks`] keeps it: the
    /// differ's state is the rows' text (`AltArchiveDiffer::prev`/`below`), which dropping link
    /// destinations leaves unchanged.
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) const fn keeps_differ_state(self) -> bool {
        matches!(self, Self::Full | Self::VisibleOnly | Self::StrippedLinks)
    }
}

#[cfg(any(unix, test))]
impl std::fmt::Display for CarryRung {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Full => "full",
            Self::VisibleOnly => "visible-only",
            Self::StrippedLinks => "stripped-links",
            Self::Sanitized => "sanitized",
            Self::Repaint => "repaint",
        })
    }
}

/// Clamp every scalar field of `checkpoint` that breaks one of this build's meta bounds, one
/// field at a time in the consumer's order; the grids are never touched. Returns the violations
/// clamped, or why the checkpoint cannot be sanitized (a bound with no clamp, or a field still
/// over its bound after its clamp), which sends the carry to Repaint. No honest engine breaks a
/// meta bound; this rung covers a future producer/predicate disagreement at least cost.
#[cfg(any(unix, test))]
pub(crate) fn sanitize_checkpoint_for_wire(
    checkpoint: &mut TerminalCheckpoint,
) -> Result<Vec<MetaBoundViolation>, String> {
    let mut clamped: Vec<MetaBoundViolation> = Vec::new();
    loop {
        let Some(violation) =
            checkpoint_meta_bound_violation(&CheckpointMeta::from_checkpoint(checkpoint))
        else {
            return Ok(clamped);
        };
        if clamped
            .iter()
            .any(|done| (done.slot, done.field) == (violation.slot, violation.field))
        {
            return Err(format!("{violation} survived its own clamp"));
        }
        let unsanitizable = || format!("{violation} has no sanitizer");
        let (rows, cols) = (checkpoint.rows, checkpoint.cols);
        match violation.slot {
            "cursor" | "alt_cursor" => {
                let cursor = if violation.slot == "cursor" {
                    Some(&mut checkpoint.cursor)
                } else {
                    checkpoint.alt_cursor.as_mut()
                }
                .ok_or_else(unsanitizable)?;
                match violation.field {
                    "cursor_row" => cursor.cursor_row = rows.saturating_sub(1),
                    "cursor_col" => cursor.cursor_col = cols.saturating_sub(1),
                    "scroll_top" | "scroll_bottom" => {
                        cursor.scroll_top = 0;
                        cursor.scroll_bottom = rows.saturating_sub(1);
                    }
                    "margin_left" | "margin_right" => {
                        cursor.margin_left = 0;
                        cursor.margin_right = cols.saturating_sub(1);
                    }
                    // The engine's own default stops: one every eight columns.
                    "tab_stops.len()" => {
                        cursor.tab_stops = default_tab_stops(usize::from(cols));
                        cursor.tab_defaults_suppressed = false;
                    }
                    _ => return Err(unsanitizable()),
                }
            }
            "saved_cursor_main" | "saved_cursor_alt" => {
                let saved = if violation.slot == "saved_cursor_main" {
                    checkpoint.saved_cursor_main.as_mut()
                } else {
                    checkpoint.saved_cursor_alt.as_mut()
                }
                .ok_or_else(unsanitizable)?;
                match violation.field {
                    "cursor_row" => saved.cursor_row = aterm_core::grid::MAX_GRID_ROWS - 1,
                    "cursor_col" => saved.cursor_col = aterm_core::grid::MAX_GRID_COLS - 1,
                    _ => return Err(unsanitizable()),
                }
            }
            "" if violation.field.starts_with("current_working_directory") => {
                checkpoint.current_working_directory = None;
            }
            _ => return Err(unsanitizable()),
        }
        clamped.push(violation);
    }
}

/// Log a carry below Full: a real loss to the user (scrollback, a cursor position, or the whole
/// screen until the program redraws).
#[cfg(any(unix, test))]
fn log_degraded_carry(local_id: u64, rung: CarryRung, cause: &str) {
    if rung != CarryRung::Full {
        aterm_log::warn!("update apply: session {local_id} carried degraded ({rung}): {cause}");
    }
}

/// A 1x1 blank screen charged to no budget: the last resort when not even that fits the
/// aggregate earlier sessions spent. [`settle_wire_carries`] then finds the pool over the
/// aggregate and lowers its largest session, so this never reaches the wire unpriced. It keeps
/// `meta`'s title and shell-integration authority.
fn unadmitted_blank_carry(meta: &CheckpointMeta) -> TerminalCheckpoint {
    keep_carried_title(
        keep_shell_integration_authority(
            aterm_core::terminal::Terminal::new(1, 1)
                .checkpoint_carry_abandoning_partial(0)
                .0,
            meta,
        ),
        meta,
    )
}

/// Copy `meta`'s titles onto a fresh engine's `blank` checkpoint. The program redraws its
/// screen but sets its title once, at start, so a blank fallback must not say "untitled".
/// `None` stays `None`, so the successor's record fallback still applies; the adopting engine
/// bounds it (`Terminal::restore_title`).
fn keep_carried_title(mut blank: TerminalCheckpoint, meta: &CheckpointMeta) -> TerminalCheckpoint {
    blank.title.clone_from(&meta.title);
    blank
}

/// Copy `meta`'s `require_shell_integration_nonce` and nonce onto a fresh engine's `blank`
/// checkpoint. The shell keeps signing its OSC 133/633 marks with its spawn nonce whatever
/// happened to the screen, so carrying the fresh engine's "not required" would let any
/// program's output forge command marks and exit codes (`spawn::authorize_adopted_shell_nonce`).
/// Only the authority is kept; the screen state stays the fresh engine's.
fn keep_shell_integration_authority(
    mut blank: TerminalCheckpoint,
    meta: &CheckpointMeta,
) -> TerminalCheckpoint {
    blank.modes.require_shell_integration_nonce = meta.modes.require_shell_integration_nonce;
    blank.shell_integration_nonce = meta
        .shell_integration_nonce
        .as_deref()
        .and_then(aterm_core::terminal::ShellIntegrationNonce::from_hex);
    blank
}

/// The Repaint rung for a session whose scalar state is `meta`: that state, sanitized, on a
/// blank canonical screen, priced against `aggregate_cells` at the successor's `caps`
/// ([`repaint_checkpoint_within`]). It reads no grid, so a caller without a projection can pass
/// [`Terminal::carry_meta_abandoning_partial`]'s meta.
///
/// [`Terminal::carry_meta_abandoning_partial`]:
///     aterm_core::terminal::Terminal::carry_meta_abandoning_partial
#[cfg(any(unix, test))]
fn repaint_carry(
    meta: &CheckpointMeta,
    aggregate_cells: &mut u64,
    caps: WireCaps,
) -> TerminalCheckpoint {
    repaint_checkpoint_within(Some(meta), aggregate_cells, caps)
        .unwrap_or_else(|| unadmitted_blank_carry(meta))
}

/// The first grid blob of `checkpoint` over the byte cap its own geometry and
/// carried history price — the `GridOverCap` arms of `screen_digest`, asked
/// while the carry can still be lowered.
#[cfg(any(unix, test))]
fn grid_over_cap(local_id: u64, checkpoint: &TerminalCheckpoint) -> Option<ScreenDigestRefusal> {
    let cap = dimension_grid_cap(checkpoint.rows, checkpoint.cols, checkpoint.history_lines)?;
    [
        (GridSlot::Main, Some(&checkpoint.grid)),
        (GridSlot::Inactive, checkpoint.alt_grid.as_ref()),
    ]
    .into_iter()
    .find_map(|(which, blob)| {
        let len = u64::try_from(blob?.len()).unwrap_or(u64::MAX);
        (len > cap).then_some(ScreenDigestRefusal::GridOverCap {
            local_id,
            which,
            len,
            cap,
        })
    })
}

/// The grid predicates one carry must pass at the successor's `caps`, or `None`: this build's
/// shape check (parser Ground, the inactive grid paired with its cursor, both blobs canonical at
/// the declared record count, so every line record is under [`line_record_cap`]), the grid byte
/// caps, and the successor's own line decoder where it is older than this build's
/// ([`WireCaps::line_decoder_refusal`]). Every grid-exact rung of [`carry_for_wire`] asks this,
/// in `screen_digest`'s order.
#[cfg(any(unix, test))]
fn grid_refusal(local_id: u64, checkpoint: &TerminalCheckpoint, caps: WireCaps) -> Option<String> {
    checkpoint_shape_refusal(local_id, checkpoint)
        .or_else(|| grid_over_cap(local_id, checkpoint))
        .map(|refusal| refusal.to_string())
        .or_else(|| {
            caps.line_decoder_refusal(checkpoint)
                .map(|why| format!("session {local_id}: {why}"))
        })
}

/// The meta half of a grid-exact rung: clamp whatever scalar breaks a meta bound
/// ([`sanitize_checkpoint_for_wire`]), lowering the carry to at least [`CarryRung::Sanitized`]
/// and saying so in `causes`. `None` when a bound has no clamp; the carry then goes to Repaint.
#[cfg(any(unix, test))]
fn clamp_carry_meta(
    checkpoint: &mut TerminalCheckpoint,
    rung: CarryRung,
    causes: &mut Vec<String>,
) -> Option<CarryRung> {
    if checkpoint_meta_bound_violation(&CheckpointMeta::from_checkpoint(checkpoint)).is_none() {
        return Some(rung);
    }
    match sanitize_checkpoint_for_wire(checkpoint) {
        Ok(clamped) => {
            causes.push(format!(
                "meta out of bounds at {}x{}, clamped: {}",
                checkpoint.rows,
                checkpoint.cols,
                clamped
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            Some(rung.max(CarryRung::Sanitized))
        }
        Err(why) => {
            causes.push(why);
            None
        }
    }
}

/// Commit one carry [`carry_for_wire`] chose: charge the aggregate what its
/// admission priced, log the rung when it is below Full, and name the causes.
#[cfg(any(unix, test))]
fn commit_carry(
    local_id: u64,
    checkpoint: TerminalCheckpoint,
    rung: CarryRung,
    charged: u64,
    aggregate_cells: &mut u64,
    causes: &[String],
) -> (TerminalCheckpoint, CarryRung, Option<String>) {
    *aggregate_cells = charged;
    let cause = (!causes.is_empty()).then(|| causes.join("; "));
    log_degraded_carry(local_id, rung, cause.as_deref().unwrap_or_default());
    (checkpoint, rung, cause)
}

/// The link-stripping rung's transform: drop the hyperlinks of every line record in
/// `checkpoint`'s grids that is over [`line_record_cap`], and nothing else, returning how many
/// lines lost them; `None`, with `checkpoint` untouched, when none did. An OSC 8 URL may be
/// 8 KiB and a row of one-cell links carries each URL once per span (eight links in 80 columns
/// make a 65 KiB record against a 57 KiB cap), and one such record makes the whole blob
/// non-canonical. The stripped lines keep their text, attributes, underline colours and wrap
/// flag ([`Line::clear_hyperlinks`]); the caller re-judges the result ([`grid_refusal`]).
///
/// [`Line::clear_hyperlinks`]: aterm_core::scrollback::Line::clear_hyperlinks
#[cfg(any(unix, test))]
fn strip_over_cap_links(checkpoint: &mut TerminalCheckpoint) -> Option<usize> {
    strip_affordable(checkpoint).ok()?;
    let record_cap = line_record_cap(checkpoint.cols);
    let rows = usize::from(checkpoint.rows);
    let main = usize::try_from(checkpoint.history_lines)
        .ok()
        .and_then(|history| rows.checked_add(history))
        .and_then(|records| strip_blob_links(&checkpoint.grid, records, record_cap));
    // The inactive grid never carries history.
    let alt = checkpoint
        .alt_grid
        .as_deref()
        .and_then(|alt| strip_blob_links(alt, rows, record_cap));
    let mut lost = 0_usize;
    if let Some((grid, lines)) = main {
        checkpoint.grid = grid;
        lost = lost.saturating_add(lines);
    }
    if let Some((alt_grid, lines)) = alt {
        checkpoint.alt_grid = Some(alt_grid);
        lost = lost.saturating_add(lines);
    }
    (lost > 0).then_some(lost)
}

/// At most this many grid bytes may be decoded and re-serialized by one link-stripping attempt
/// while every PTY reader is parked. The geometry price does not bound OSC 8 URL bytes: a 24x80
/// pane can hold megabytes of one-cell links. 4 MiB admits that pane's 24 rows of 16 links with
/// 8 KiB URLs; a larger one falls to Repaint. [`strip_every_link`] (the held-pane reader) is
/// bounded by it too, on every platform.
const MAX_LINK_STRIP_BYTES: u64 = 4 * 1024 * 1024;

/// Whether the link-stripping rung may decode `checkpoint`'s grids at all: bounded by their
/// actual bytes for every carry and, with scrollback, by what the capture priced that depth at
/// ([`checkpoint_capture_budget_bytes`]); else the cause. The strip costs a few times the blob
/// inside the frozen window, where the capture checks its deadline only between sessions, and
/// scrollback is priced by dimensions, not bytes, so output alone (rows of one-cell links with
/// 8 KiB URLs) can make a depth hundreds of megabytes. Such a depth goes on to the screen
/// without scrollback; a visible screen past the absolute bound goes to Repaint.
#[cfg(any(unix, test))]
fn strip_affordable(checkpoint: &TerminalCheckpoint) -> Result<(), String> {
    let held = wire_bytes(checkpoint);
    if held > MAX_LINK_STRIP_BYTES {
        return Err(format!(
            "its grids hold {held} bytes, past the {MAX_LINK_STRIP_BYTES}-byte link-strip work \
             bound, so they are not decoded to strip links"
        ));
    }
    if checkpoint.history_lines == 0 {
        return Ok(());
    }
    let priced =
        checkpoint_capture_budget_bytes(checkpoint.rows, checkpoint.cols, checkpoint.history_lines)
            .unwrap_or(0);
    if held > priced {
        return Err(format!(
            "with {} history line(s) its grids hold {held} bytes, past the {priced} this \
             capture prices that depth at, so they are not decoded to strip links",
            checkpoint.history_lines
        ));
    }
    Ok(())
}

/// One grid blob of `records` line records with the hyperlinks of every record over
/// `record_cap` dropped, re-serialized, and how many records lost them; `None` when none did, or
/// when the blob is not exactly `records` records the block decoder reads back losslessly. The
/// decode is lenient because the strict decoder refuses the over-cap records this is for, so
/// every decoded line must re-serialize to the bytes it was read from: an unstripped line is
/// carried byte-for-byte, and the walk yields each record's true size for the cap.
#[cfg(any(unix, test))]
fn strip_blob_links(blob: &[u8], records: usize, record_cap: usize) -> Option<(Vec<u8>, usize)> {
    let header = u32::try_from(records).ok()?.to_le_bytes();
    if blob.get(..4)? != header.as_slice() {
        return None;
    }
    let mut lines = aterm_core::scrollback::deserialize_lines(blob);
    if lines.len() != records {
        return None;
    }
    let mut offset = header.len();
    let mut lost = 0_usize;
    for line in &mut lines {
        let record = line.serialize();
        let end = offset.checked_add(record.len())?;
        if blob.get(offset..end)? != record.as_slice() {
            return None;
        }
        offset = end;
        if record.len() > record_cap && line.has_hyperlinks() {
            line.clear_hyperlinks();
            lost = lost.saturating_add(1);
        }
    }
    (offset == blob.len() && lost > 0)
        .then(|| (aterm_core::scrollback::serialize_lines(&lines), lost))
}

/// The link-stripping rung at one scrollback depth: drop the links of `checkpoint`'s over-cap
/// line records ([`strip_over_cap_links`]) and re-judge it ([`grid_refusal`]). `true` when the
/// stripped carry passes, with `causes` saying how many lines lost links; `false` when no line
/// had any or the stripped carry is still refused (named in `causes`). `checkpoint` may then
/// hold the stripped grids, which is harmless: the Repaint rung reads only its scalars.
#[cfg(any(unix, test))]
fn strip_links_for_wire(
    local_id: u64,
    checkpoint: &mut TerminalCheckpoint,
    caps: WireCaps,
    causes: &mut Vec<String>,
) -> bool {
    if let Err(why) = strip_affordable(checkpoint) {
        causes.push(why);
        return false;
    }
    let Some(lines) = strip_over_cap_links(checkpoint) else {
        return false;
    };
    let cap = line_record_cap(checkpoint.cols);
    if let Some(refusal) = grid_refusal(local_id, checkpoint, caps) {
        causes.push(format!(
            "with the links of its {lines} line(s) over the {cap}-byte line-record cap \
             dropped, still: {refusal}"
        ));
        return false;
    }
    causes.push(format!(
        "{lines} line(s) over the {cap}-byte line-record cap lost their links; their text is \
         exact"
    ));
    true
}

/// The producer's ladder for one session: the most faithful carry of `terminal` this build's
/// wire predicates admit at the successor's `caps`, its rung, and why it is below Full. Total:
/// every failed predicate picks the next rung, and the last is a blank canonical screen.
///
/// The rungs, in order: the screen with `want_history` lines of scrollback
/// ([`CarryRung::Full`]); that with its over-cap line records' links dropped
/// ([`CarryRung::StrippedLinks`]); the screen without scrollback ([`CarryRung::VisibleOnly`]);
/// that with its over-cap links dropped; any of these with out-of-bound scalars clamped
/// ([`CarryRung::Sanitized`]); a blank screen carrying the sanitized scalars
/// ([`CarryRung::Repaint`]). Each is judged by the predicates `screen_digest` and the consumer
/// run: [`WireCaps::admit`] (cells, charged to `aggregate_cells` only for the carry returned),
/// [`checkpoint_shape_refusal`], the grid byte caps, [`WireCaps::line_decoder_refusal`] and
/// [`checkpoint_meta_bound_violation`].
///
/// The links go before the scrollback although StrippedLinks ranks below VisibleOnly: at a given
/// depth the strip loses less than dropping the depth, and once one session lands on VisibleOnly
/// the capture latches history off for every later one
/// (`app_update_handoff::capture_parked_screens`). The rank is what [`CarryRung`]'s `max`
/// combines when the self-check later drops a stripped carry's scrollback.
///
/// The inactive grid is charged only when the engine has one ([`Terminal::has_inactive_grid`]),
/// as `screen_digest` and the successor charge it.
///
/// A parser left mid-sequence does not refuse: [`Terminal::checkpoint_carry_abandoning_partial`]
/// carries the partial sequence (`TerminalCheckpoint::parser`) for the successor to continue,
/// leaves out only a hooked DCS query (as CAN would), and never touches the live parser, so a
/// rolled-back handoff resumes byte-exactly. Whether abandoning a hooked DCS is safe is the
/// capture's question, asked first (`app_update_handoff::CaptureFailure::MidSequence`).
///
/// [`Terminal::has_inactive_grid`]: aterm_core::terminal::Terminal::has_inactive_grid
/// [`Terminal::checkpoint_carry_abandoning_partial`]:
///     aterm_core::terminal::Terminal::checkpoint_carry_abandoning_partial
#[cfg(any(unix, test))]
pub(crate) fn carry_for_wire(
    terminal: &aterm_core::terminal::Terminal,
    local_id: u64,
    want_history: u32,
    aggregate_cells: &mut u64,
    caps: WireCaps,
) -> (TerminalCheckpoint, CarryRung, Option<String>) {
    let (rows, cols) = (terminal.rows(), terminal.cols());
    let want_history = want_history.min(MAX_HANDOFF_HISTORY_LINES);
    // What the projection will carry as `alt_grid`, read before paying for it.
    let has_alt = terminal.has_inactive_grid();
    let mut causes: Vec<String> = Vec::new();
    let mut projection = None;
    let mut told_partial = false;
    let ladder = [Some(want_history), (want_history > 0).then_some(0)];
    for history in ladder.into_iter().flatten() {
        let mut charged = *aggregate_cells;
        if let Err(bound) = caps.admit(&mut charged, rows, cols, history, has_alt) {
            causes.push(format!(
                "{rows}x{cols} with {history} history line(s) was refused: {bound}"
            ));
            continue;
        }
        let (mut checkpoint, abandoned) =
            terminal.checkpoint_carry_abandoning_partial(history as usize);
        if let Some(state) = abandoned
            && !told_partial
        {
            told_partial = true;
            aterm_log::info!(
                "update apply: session {local_id}'s parser was mid-sequence ({state}); its \
                 carry leaves that partial sequence out, as CAN would"
            );
        }
        let rung = match grid_refusal(local_id, &checkpoint, caps) {
            None if history == want_history => CarryRung::Full,
            None => CarryRung::VisibleOnly,
            Some(refusal) => {
                causes.push(refusal);
                if !strip_links_for_wire(local_id, &mut checkpoint, caps, &mut causes) {
                    projection = Some(checkpoint);
                    continue;
                }
                CarryRung::StrippedLinks
            }
        };
        match clamp_carry_meta(&mut checkpoint, rung, &mut causes) {
            Some(rung) => {
                return commit_carry(
                    local_id,
                    checkpoint,
                    rung,
                    charged,
                    aggregate_cells,
                    &causes,
                );
            }
            // No grid-exact rung can fix a scalar with no clamp.
            None => {
                projection = Some(checkpoint);
                break;
            }
        }
    }
    // The scalars of the projection the ladder last refused, or — when every
    // depth was refused before anything was projected — the engine's own, read
    // without projecting a grid.
    let meta = projection.as_ref().map_or_else(
        || terminal.carry_meta_abandoning_partial(),
        CheckpointMeta::from_checkpoint,
    );
    let checkpoint = repaint_carry(&meta, aggregate_cells, caps);
    let cause = causes.join("; ");
    log_degraded_carry(local_id, CarryRung::Repaint, &cause);
    (checkpoint, CarryRung::Repaint, Some(cause))
}

/// The Repaint rung taken directly: for a session the capture loop's own decode-authority budget
/// (`MAX_HANDOFF_CAPTURE_BUDGET_BYTES`, priced only by the producer) cannot afford at a higher
/// rung, and for every session under a successor's `carry = "repaint"` policy. No grid is
/// projected ([`Terminal::carry_meta_abandoning_partial`]), so the policy that routes around a
/// slow or panicking grid serializer never runs it.
///
/// [`Terminal::carry_meta_abandoning_partial`]:
///     aterm_core::terminal::Terminal::carry_meta_abandoning_partial
#[cfg(any(unix, test))]
pub(crate) fn repaint_carry_for_wire(
    terminal: &aterm_core::terminal::Terminal,
    local_id: u64,
    aggregate_cells: &mut u64,
    caps: WireCaps,
    cause: String,
) -> (TerminalCheckpoint, CarryRung, Option<String>) {
    let meta = terminal.carry_meta_abandoning_partial();
    let checkpoint = repaint_carry(&meta, aggregate_cells, caps);
    log_degraded_carry(local_id, CarryRung::Repaint, &cause);
    (checkpoint, CarryRung::Repaint, Some(cause))
}

/// [`carry_for_wire`] under the ceiling the successor's signed handoff policy sets
/// ([`aterm_update_core::handoff_policy`]): the lower of the ladder's rung and the policy's.
///
/// * [`CarryCeiling::Full`](aterm_update_core::handoff_policy::CarryCeiling::Full): exactly
///   [`carry_for_wire`].
/// * `Visible`: the ladder with no scrollback asked for; a carry it would call
///   [`CarryRung::Full`] is labelled [`CarryRung::VisibleOnly`]. Every lower rung stands.
/// * `Repaint`: the blank canonical screen ([`repaint_carry_for_wire`]) for every session.
///
/// Only ever lower: the policy chooses among rungs this build's predicates already admit, so the
/// carry is one this producer could have made without it. That is why the ceiling may come from
/// the successor's bundle (the policy module has the trust argument).
#[cfg(any(unix, test))]
pub(crate) fn carry_for_wire_within(
    terminal: &aterm_core::terminal::Terminal,
    local_id: u64,
    want_history: u32,
    aggregate_cells: &mut u64,
    caps: WireCaps,
    ceiling: aterm_update_core::handoff_policy::CarryCeiling,
) -> (TerminalCheckpoint, CarryRung, Option<String>) {
    use aterm_update_core::handoff_policy::CarryCeiling;
    match ceiling {
        CarryCeiling::Full => {
            carry_for_wire(terminal, local_id, want_history, aggregate_cells, caps)
        }
        CarryCeiling::Visible => {
            let (checkpoint, rung, cause) =
                carry_for_wire(terminal, local_id, 0, aggregate_cells, caps);
            (checkpoint, rung.max(CarryRung::VisibleOnly), cause)
        }
        CarryCeiling::Repaint => repaint_carry_for_wire(
            terminal,
            local_id,
            aggregate_cells,
            caps,
            "the successor's handoff policy carries every session as a blank screen its \
             program redraws"
                .to_string(),
        ),
    }
}

/// One session's screen as the producer will commit it, and the rung it was
/// carried at — the unit [`settle_wire_carries`] lowers.
#[derive(Debug, Clone)]
#[cfg(any(unix, test))]
pub(crate) struct WireCarry {
    pub(crate) local_id: u64,
    pub(crate) checkpoint: TerminalCheckpoint,
    pub(crate) rung: CarryRung,
}

/// What one carry costs `screen_digest`'s aggregate cell admission — the same
/// sum [`WireCaps::admit`] charges, with the inactive grid counted only when it
/// is really carried.
#[cfg(any(unix, test))]
fn wire_cells(checkpoint: &TerminalCheckpoint) -> u64 {
    let visible = u64::from(checkpoint.rows).saturating_mul(u64::from(checkpoint.cols));
    let history = u64::from(checkpoint.history_lines).saturating_mul(u64::from(checkpoint.cols));
    visible
        .saturating_add(history)
        .saturating_add(if checkpoint.alt_grid.is_some() {
            visible
        } else {
            0
        })
}

/// The real bytes one carry puts on the wire — what the 256 MiB aggregate
/// (`MAX_HANDOFF_AGGREGATE_GRID_BYTES`) counts.
#[cfg(any(unix, test))]
fn wire_bytes(checkpoint: &TerminalCheckpoint) -> u64 {
    let len = |blob: &[u8]| u64::try_from(blob.len()).unwrap_or(u64::MAX);
    len(&checkpoint.grid).saturating_add(checkpoint.alt_grid.as_deref().map_or(0, len))
}

/// Re-encode a carried checkpoint's main grid without its scrollback: the last `rows` records of
/// its own strict decode, re-serialized, so the result is canonical by construction and needs no
/// engine lock. `false` (and nothing changed) when there is no history to drop or the blob does
/// not decode.
#[cfg(any(unix, test))]
fn drop_carried_history(checkpoint: &mut TerminalCheckpoint) -> bool {
    if checkpoint.history_lines == 0 {
        return false;
    }
    let Some(lines) = strict_grid_lines(
        &checkpoint.grid,
        checkpoint.rows,
        checkpoint.cols,
        checkpoint.history_lines,
    ) else {
        return false;
    };
    let Some(visible) = lines.get(lines.len().saturating_sub(usize::from(checkpoint.rows))..)
    else {
        return false;
    };
    checkpoint.grid = aterm_core::scrollback::serialize_lines(visible);
    checkpoint.history_lines = 0;
    true
}

/// Whether a carry has a rung below it: anything above Repaint (a
/// [`CarryRung::StrippedLinks`] carry included — its scrollback, then its
/// screen, can still go), and a Repaint carry bigger than the smallest blank
/// screen (a pool-wide refusal may still shrink it — 24x80 at most, then 1x1 —
/// as [`repaint_checkpoint_within`]'s own fallback does).
#[cfg(any(unix, test))]
fn is_lowerable(carry: &WireCarry) -> bool {
    carry.rung != CarryRung::Repaint || (carry.checkpoint.rows, carry.checkpoint.cols) != (1, 1)
}

/// The index of the carry a pool-wide refusal should lower: the lowerable one costing the most
/// by `cost`.
#[cfg(any(unix, test))]
fn largest_lowerable(carries: &[WireCarry], cost: fn(&TerminalCheckpoint) -> u64) -> Option<usize> {
    carries
        .iter()
        .enumerate()
        .filter(|(_, carry)| is_lowerable(carry))
        .max_by_key(|(_, carry)| cost(&carry.checkpoint))
        .map(|(index, _)| index)
}

/// Lower one carry a single step, priced against every other carry's cells: without `repaint`,
/// drop its scrollback if it has some (a [`CarryRung::StrippedLinks`] carry stays at that rung);
/// with `repaint` and a carry still exact, drop its over-cap line records' links if that alone
/// makes its grids pass ([`strip_over_cap_links`]), so a carry blamed for a link-dense line loses
/// that line's links, not its screen; else carry it at Repaint; and a carry already there becomes
/// a smaller blank screen (24x80 at most, then 1x1).
#[cfg(any(unix, test))]
fn lower_wire_carry(
    carries: &mut [WireCarry],
    index: usize,
    repaint: bool,
    caps: WireCaps,
    why: &str,
) {
    let others = carries
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .map(|(_, carry)| wire_cells(&carry.checkpoint))
        .fold(0_u64, u64::saturating_add);
    let Some(carry) = carries.get_mut(index) else {
        return;
    };
    if carry.rung != CarryRung::Repaint && !repaint && drop_carried_history(&mut carry.checkpoint) {
        carry.rung = carry.rung.max(CarryRung::VisibleOnly);
        log_degraded_carry(
            carry.local_id,
            carry.rung,
            &format!("the capture's self-check dropped its scrollback: {why}"),
        );
        return;
    }
    // The salvage path clones the whole blob before the strip, so refuse over-budget work
    // before that copy, as the ladder does before decoding.
    if repaint
        && carry.rung < CarryRung::StrippedLinks
        && strip_affordable(&carry.checkpoint).is_ok()
    {
        let mut stripped = carry.checkpoint.clone();
        let mut said = Vec::new();
        if strip_links_for_wire(carry.local_id, &mut stripped, caps, &mut said) {
            carry.checkpoint = stripped;
            carry.rung = CarryRung::StrippedLinks;
            log_degraded_carry(
                carry.local_id,
                carry.rung,
                &format!(
                    "the capture's self-check refused its carry: {why}; {}",
                    said.join("; ")
                ),
            );
            return;
        }
    }
    let mut meta = CheckpointMeta::from_checkpoint(&carry.checkpoint);
    if carry.rung == CarryRung::Repaint {
        (meta.rows, meta.cols) = if meta.rows > 24 || meta.cols > 80 {
            (meta.rows.min(24), meta.cols.min(80))
        } else {
            (1, 1)
        };
    }
    let mut used = others;
    carry.checkpoint = repaint_checkpoint_within(Some(&meta), &mut used, caps)
        .unwrap_or_else(|| unadmitted_blank_carry(&meta));
    carry.rung = CarryRung::Repaint;
    log_degraded_carry(
        carry.local_id,
        carry.rung,
        &format!("the capture's self-check refused its carry: {why}"),
    );
}

/// The producer's self-check: commit the pool with `screen_digest` (this build's full predicate,
/// the one its consumer runs) and where it refuses, lower the carry it blames and commit again
/// instead of refusing the update.
///
/// * A refusal naming a session replaces that session's carry with its Repaint carry, at most
///   once, unless dropping its over-cap line records' links makes its grids pass: then it is
///   carried at StrippedLinks, also at most once.
/// * The pool over the aggregate cell budget, or over the 256 MiB of real grid bytes, lowers its
///   largest session: its scrollback first, then its screen.
/// * Metas over their share of the manifest ([`MAX_HANDOFF_AGGREGATE_META_BYTES`]) shed what
///   the screens do not need ([`shed_metas_to_budget`]); with nothing left to shed, the session
///   whose meta costs the most has its screen lowered.
/// * What survives is typed: too many sessions, a duplicate id, an allocation failure, a refusal
///   of a carry already at Repaint (admitted by construction; a test pins it unreachable), and
///   metas still over their share once every screen is a 1x1 blank.
///
/// Bounded: every lowering moves one carry strictly down (its history, its over-cap links, its
/// screen, a blank screen to 24x80, then 1x1, each once), so at most five per session happen.
#[cfg(any(unix, test))]
pub(crate) fn settle_wire_carries(
    carries: &mut [WireCarry],
    caps: WireCaps,
) -> Result<[u8; 32], ScreenDigestRefusal> {
    let mut lowerings_left = carries.len().saturating_mul(5);
    // At most once more than the lowerings: every shed pass that changed
    // something is followed by one more digest, and a pass that changes
    // nothing ends the settle.
    let mut meta_passes_left = 2_u8;
    loop {
        let refusal = match screen_digest_and_meta_cost(
            carries
                .iter()
                .map(|carry| (carry.local_id, &carry.checkpoint))
                .collect(),
        ) {
            Ok((digest, meta_cost)) if meta_cost <= MAX_HANDOFF_AGGREGATE_META_BYTES => {
                return Ok(digest);
            }
            // Over the metas' share of the manifest: shed the largest metas and commit again,
            // so the digest covers the metas as they will be written.
            Ok((digest, _)) => match shed_metas_to_budget(carries) {
                MetaBudget::Fits => return Ok(digest),
                MetaBudget::Shed if meta_passes_left > 0 => {
                    meta_passes_left -= 1;
                    continue;
                }
                MetaBudget::Shed => return Ok(digest),
                // Every shed spent and still over: lower the screen of the carry whose meta
                // costs the most (a smaller blank screen needs fewer tab stops, which the next
                // shed cuts) rather than refuse the update on content. It is charged to the
                // lowerings; when none is left, the refusal stands.
                MetaBudget::Over { cost } => match largest_lowerable(carries, meta_manifest_cost)
                    .filter(|_| lowerings_left > 0)
                {
                    Some(index) => {
                        lowerings_left -= 1;
                        let why = format!(
                            "the screens' metas cost the manifest {cost} bytes with nothing \
                             left to shed, over its {MAX_HANDOFF_AGGREGATE_META_BYTES}-byte \
                             budget"
                        );
                        lower_wire_carry(carries, index, true, caps, &why);
                        continue;
                    }
                    None => {
                        return Err(ScreenDigestRefusal::MetasOverBudget {
                            cost,
                            budget: MAX_HANDOFF_AGGREGATE_META_BYTES,
                        });
                    }
                },
            },
            Err(refusal) => refusal,
        };
        if lowerings_left == 0 {
            return Err(refusal);
        }
        lowerings_left -= 1;
        let (index, repaint) = match &refusal {
            ScreenDigestRefusal::TooManySessions { .. }
            | ScreenDigestRefusal::DuplicateLocalId { .. }
            | ScreenDigestRefusal::Alloc { .. } => return Err(refusal),
            ScreenDigestRefusal::DimensionsRefused {
                bound: AdmitRefusal::Aggregate { .. },
                ..
            } => match largest_lowerable(carries, wire_cells) {
                Some(index) => (index, false),
                None => return Err(refusal),
            },
            ScreenDigestRefusal::WireFraming => {
                let total = carries
                    .iter()
                    .map(|carry| wire_bytes(&carry.checkpoint))
                    .fold(0_u64, u64::saturating_add);
                if total <= MAX_HANDOFF_AGGREGATE_GRID_BYTES {
                    return Err(refusal);
                }
                match largest_lowerable(carries, wire_bytes) {
                    Some(index) => (index, false),
                    None => return Err(refusal),
                }
            }
            blamed => {
                let Some(position) = blamed.local_id().and_then(|local_id| {
                    carries.iter().position(|carry| {
                        carry.local_id == local_id && carry.rung != CarryRung::Repaint
                    })
                }) else {
                    return Err(refusal);
                };
                (position, true)
            }
        };
        let why = if matches!(refusal, ScreenDigestRefusal::WireFraming) {
            format!(
                "the pool's grids are over the {MAX_HANDOFF_AGGREGATE_GRID_BYTES}-byte aggregate"
            )
        } else {
            refusal.to_string()
        };
        lower_wire_carry(carries, index, repaint, caps, &why);
    }
}

/// What one meta costs the manifest as TOML writes it: its `aterm_json` bytes,
/// plus one byte per character a TOML basic string must escape (`"`, `\`),
/// plus five per control character (`\uXXXX`) — an upper bound whichever
/// string form the writer picks. `u64::MAX` for a meta that will not
/// serialize (the digest refuses that one by name first).
#[cfg(any(unix, test))]
fn meta_manifest_cost(checkpoint: &TerminalCheckpoint) -> u64 {
    aterm_json::to_vec(&CheckpointMeta::from_checkpoint(checkpoint))
        .map_or(u64::MAX, |json| meta_json_manifest_cost(&json))
}

/// [`meta_manifest_cost`] of meta bytes already serialized.
#[cfg(any(unix, test))]
fn meta_json_manifest_cost(json: &[u8]) -> u64 {
    json.iter()
        .map(|byte| match byte {
            b'"' | b'\\' => 2_u64,
            0..=0x1f | 0x7f => 6,
            _ => 1,
        })
        .fold(0_u64, u64::saturating_add)
}

/// What [`shed_metas_to_budget`] did.
#[cfg(any(unix, test))]
enum MetaBudget {
    /// The metas already fit: nothing changed.
    Fits,
    /// Marks were shed until they fit; the pool must be committed again.
    Shed,
    /// Every carried mark is gone and the metas still cost `cost`.
    Over { cost: u64 },
}

/// Bring the pool's metas under [`MAX_HANDOFF_AGGREGATE_META_BYTES`] by lowering the largest
/// meta one step at a time: its completed command marks and output blocks (the carried OSC 133
/// record, the bulk of a shell tab's meta); the mark and block being built; its colour and title
/// stacks (a kitty colour stack serializes all 256 palette entries per push); its tab stops past
/// its width (a grid never shrinks its stop vector); then the payload of the OSC sequence its
/// parser is inside (up to 4 KiB, carried as hex), which is then discarded at its terminator as
/// an over-cap OSC is. The screen is untouched, so nothing needs a repaint; each loss is logged
/// per session. Each carry lowers at most five times; past that, [`settle_wire_carries`] lowers
/// screens.
#[cfg(any(unix, test))]
fn shed_metas_to_budget(carries: &mut [WireCarry]) -> MetaBudget {
    let mut costs = carries
        .iter()
        .map(|carry| meta_manifest_cost(&carry.checkpoint))
        .collect::<Vec<_>>();
    let mut total = costs.iter().copied().fold(0_u64, u64::saturating_add);
    let mut shed = false;
    while total > MAX_HANDOFF_AGGREGATE_META_BYTES {
        let largest = costs
            .iter()
            .enumerate()
            .filter(|(index, _)| meta_has_sheddable(&carries[*index].checkpoint))
            .max_by_key(|(_, cost)| **cost)
            .map(|(index, _)| index);
        let Some(index) = largest else {
            return MetaBudget::Over { cost: total };
        };
        let carry = &mut carries[index];
        let checkpoint = &mut carry.checkpoint;
        let shell = &mut checkpoint.shell;
        let what = if !shell.command_marks.is_empty() || !shell.output_blocks.is_empty() {
            shell.command_marks.clear();
            shell.output_blocks.clear();
            "the command marks and output blocks of its carried lines"
        } else if shell.current_mark.is_some() || shell.current_block.is_some() {
            shell.current_mark = None;
            shell.current_block = None;
            "the command mark and output block it was building"
        } else if meta_stacks_pushed(checkpoint) {
            checkpoint.color.clear_stack();
            if let Some(title) = checkpoint.title.as_mut() {
                title.stack.clear();
            }
            "its pushed colour and title stacks"
        } else if meta_tab_stops_past_width(checkpoint) {
            let cols = usize::from(checkpoint.cols);
            for cursor in std::iter::once(&mut checkpoint.cursor).chain(&mut checkpoint.alt_cursor)
            {
                cursor.tab_stops.truncate(cols);
            }
            "its tab stops past its width"
        } else {
            if let Some(parser) = checkpoint.parser.as_mut() {
                parser.osc.clear();
                parser.osc_overflowed = true;
            }
            "the payload of the OSC sequence it was in the middle of (the rest is discarded at \
             its terminator, not applied)"
        };
        aterm_log::warn!(
            "update apply: session {} carried without {what}: the screens' metas cost the \
             manifest {total} bytes, over its {MAX_HANDOFF_AGGREGATE_META_BYTES}-byte budget; \
             its screen is exact",
            carry.local_id
        );
        let cost = meta_manifest_cost(&carry.checkpoint);
        total = total.saturating_sub(costs[index]).saturating_add(cost);
        costs[index] = cost;
        shed = true;
    }
    if shed {
        MetaBudget::Shed
    } else {
        MetaBudget::Fits
    }
}

/// Whether [`shed_metas_to_budget`] has a step left for this meta.
#[cfg(any(unix, test))]
fn meta_has_sheddable(checkpoint: &TerminalCheckpoint) -> bool {
    let shell = &checkpoint.shell;
    !shell.command_marks.is_empty()
        || !shell.output_blocks.is_empty()
        || shell.current_mark.is_some()
        || shell.current_block.is_some()
        || meta_stacks_pushed(checkpoint)
        || meta_tab_stops_past_width(checkpoint)
        || checkpoint
            .parser
            .as_ref()
            .is_some_and(|parser| !parser.osc.is_empty())
}

/// Whether a meta carries a pushed colour or title stack.
#[cfg(any(unix, test))]
fn meta_stacks_pushed(checkpoint: &TerminalCheckpoint) -> bool {
    checkpoint.color.has_stack()
        || checkpoint
            .title
            .as_ref()
            .is_some_and(|title| !title.stack.is_empty())
}

/// Whether a cursor of a meta carries tab stops past the screen's width, which
/// the consumer does not need (it requires a stop for every column, no more:
/// `grid_cursor_bound_violation`).
#[cfg(any(unix, test))]
fn meta_tab_stops_past_width(checkpoint: &TerminalCheckpoint) -> bool {
    std::iter::once(&checkpoint.cursor)
        .chain(&checkpoint.alt_cursor)
        .any(|cursor| cursor.tab_stops.len() > usize::from(checkpoint.cols))
}

/// Validate the volatile fd channel as an exact bijection before any inherited
/// descriptor is adopted. Counts alone are insufficient: duplicate ids or fd
/// numbers can otherwise map two logical sessions onto one PTY reader.
fn validated_identities(
    manifest: &SessionHandoff,
    fds: &HandoffFds,
) -> Option<Vec<SessionIdentity>> {
    if manifest.sessions.len() > MAX_HANDOFF_SESSIONS
        || manifest.sessions.len() != fds.entries.len()
    {
        return None;
    }
    let mut manifest_ids = manifest
        .sessions
        .iter()
        .map(|record| record.local_id)
        .collect::<Vec<_>>();
    manifest_ids.sort_unstable();
    if manifest_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return None;
    }
    let mut identities = fds.entries.clone();
    identities.sort_unstable_by_key(|(local_id, _, _)| *local_id);
    if identities.windows(2).any(|pair| pair[0].0 == pair[1].0)
        || identities.iter().any(|(_, fd, pid)| *fd < 3 || *pid <= 0)
    {
        return None;
    }
    let mut fd_numbers = identities.iter().map(|(_, fd, _)| *fd).collect::<Vec<_>>();
    fd_numbers.sort_unstable();
    if fd_numbers.windows(2).any(|pair| pair[0] == pair[1])
        || identities
            .iter()
            .map(|(local_id, _, _)| *local_id)
            .ne(manifest_ids)
    {
        return None;
    }
    Some(identities)
}

fn parse_fd_entry(entry: &str) -> Option<SessionIdentity> {
    let (local_id, rest) = entry.split_once('=')?;
    let (fd, pid) = rest.split_once(':')?;
    Some((local_id.parse().ok()?, fd.parse().ok()?, pid.parse().ok()?))
}

/// Extract only the descriptor position from an fd-channel entry. Startup uses
/// this weaker parser solely to re-arm CLOEXEC on every syntactically named live
/// fd, even when the surrounding identity/pid makes the authority invalid.
#[cfg(unix)]
fn parse_named_fd(entry: &str) -> Option<i32> {
    let (_, rest) = entry.split_once('=')?;
    let (fd, _) = rest.split_once(':')?;
    fd.parse().ok()
}

fn decode_fds_bounded(wire: &str) -> Option<HandoffFds> {
    if wire.len() > MAX_HANDOFF_FDS_WIRE_BYTES {
        return None;
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(
            wire.matches(',')
                .count()
                .saturating_add(1)
                .min(MAX_HANDOFF_SESSIONS),
        )
        .ok()?;
    if wire.is_empty() {
        return Some(HandoffFds { entries });
    }
    for entry in wire.split(',') {
        if entry.is_empty() || entries.len() == MAX_HANDOFF_SESSIONS {
            return None;
        }
        entries.push(parse_fd_entry(entry)?);
    }
    Some(HandoffFds { entries })
}

fn manifest_path_matches_nonce(path: &std::path::Path, nonce: &str) -> bool {
    if !aterm_codec::hex::is_hex(nonce, 32) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(stem) = name
        .strip_prefix("seamless-")
        .and_then(|name| name.strip_suffix(".toml"))
    else {
        return false;
    };
    let Some((pid, file_nonce)) = stem.rsplit_once('-') else {
        return false;
    };
    !pid.is_empty() && pid.as_bytes().iter().all(u8::is_ascii_digit) && file_nonce == nonce
}

/// The most held panes one handoff carries.
pub(crate) const MAX_HELD_PANES: usize = 64;
/// The most bytes one held pane's screen meta may put in the manifest.
const MAX_HELD_META_BYTES: usize = 64 * 1024;
/// The most grid bytes all held panes together may put on disk, and that a
/// successor reads for them — on top of the handed sessions', inside the same
/// aggregate caps, and always after them.
pub(crate) const MAX_HELD_AGGREGATE_GRID_BYTES: u64 = 16 * 1024 * 1024;

/// One held pane as the park captured it: the pool id its layout leaf names, its session id,
/// its visible screen, and how its command ended when the outgoing process had collected that.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub(crate) struct HeldScreen {
    pub local_id: u64,
    pub sid: SessionId,
    pub checkpoint: TerminalCheckpoint,
    pub exit: Option<aterm_pty::ChildExit>,
    /// Its scrollback lines: what is left behind when no sidecar carries them.
    pub history_dropped: u64,
    /// The fence of its scrollback at the capture
    /// ([`crate::handoff_history::capture_head`]'s), which the held carry's
    /// join reads its export against
    /// ([`crate::handoff_history::name_held_sidecar`]); `None` carries none.
    pub history_head: Option<aterm_core::grid::HistoryFence>,
    /// Whether the person had seen the failure its exit marks the tab for
    /// ([`crate::session_status::StatusObserver::failure_seen`]).
    pub failure_seen: bool,
}

#[cfg(unix)]
impl HeldScreen {
    /// The grid bytes this screen puts on disk — what
    /// [`MAX_HELD_AGGREGATE_GRID_BYTES`] prices.
    #[must_use]
    pub(crate) fn grid_bytes(&self) -> u64 {
        let cp = &self.checkpoint;
        u64::try_from(cp.grid.len())
            .ok()
            .and_then(|main| {
                cp.alt_grid.as_ref().map_or(Some(main), |alt| {
                    u64::try_from(alt.len())
                        .ok()
                        .and_then(|alt| main.checked_add(alt))
                })
            })
            .unwrap_or(u64::MAX)
    }
}
/// The sidecar suffixes of a held pane's grids, apart from a handed session's
/// (`.grid` / `.altgrid`) so no reader of one can be pointed at the other.
const HELD_GRID_SUFFIX: &str = ".heldgrid";
const HELD_ALT_SUFFIX: &str = ".heldalt";

/// The file name of held pane `local_id`'s scrollback sidecar
/// (`crate::handoff_history::name_held_sidecar` writes it) under this
/// attempt's `stem` (`seamless-<pid>-<nonce>`).
fn held_history_name(stem: &str, local_id: u64) -> String {
    format!("{stem}.h{local_id}.hist")
}

/// The file name of held pane `local_id`'s control-carry sidecar, under this attempt's `stem`.
fn held_control_name(stem: &str, local_id: u64) -> String {
    format!("{stem}.h{local_id}.ctl")
}

/// The file name of held pane `local_id`'s grid sidecar (`alt` for its
/// inactive grid), under this attempt's `stem` (`seamless-<pid>-<nonce>`).
fn held_sidecar_name(stem: &str, local_id: u64, alt: bool) -> String {
    let suffix = if alt {
        HELD_ALT_SUFFIX
    } else {
        HELD_GRID_SUFFIX
    };
    format!("{stem}.h{local_id}{suffix}")
}

/// A held pane's screen meta as the manifest carries it, or why the writer will not carry the
/// screen whatever the budgets say: a grid past the reader's link-strip bound
/// ([`MAX_LINK_STRIP_BYTES`]) or a meta past [`MAX_HELD_META_BYTES`]. The capture asks it too,
/// so a screen the writer would drop is never projected inside the freeze.
#[cfg(unix)]
pub(crate) fn held_screen_meta(
    cp: &aterm_core::terminal::TerminalCheckpoint,
) -> Result<String, &'static str> {
    if [Some(&cp.grid), cp.alt_grid.as_ref()]
        .into_iter()
        .flatten()
        .any(|grid| u64::try_from(grid.len()).map_or(true, |len| len > MAX_LINK_STRIP_BYTES))
    {
        return Err("its grid is past the link-strip bound");
    }
    aterm_json::to_string(&CheckpointMeta::from_checkpoint(cp))
        .ok()
        .filter(|meta| meta.len() <= MAX_HELD_META_BYTES)
        .ok_or("its screen meta is past the held bound")
}

/// Write each held pane's screen the successor can read and name it in `manifest.held` (see
/// [`write_outgoing_with_held`]), with its scrollback's sidecar when `history` holds its export
/// ([`crate::handoff_history::name_held_sidecar`]). `spent` is what the handed screens already
/// took of the aggregate cell and byte caps: a held pane is priced after them, as the successor
/// reads it. A pane that is also a handed session, repeated, past [`MAX_HELD_PANES`], carrying
/// scrollback, refused by the wire's predicates or past [`held_screen_meta`]'s bounds is left
/// out, as is one whose sidecars cannot be written.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn stamp_held_panes(
    manifest: &mut SessionHandoff,
    held: &[HeldScreen],
    history: &mut crate::handoff_history::HeldExports,
    controls: &[(u64, Vec<u8>)],
    aggregate_control: &mut u64,
    dir: &std::path::Path,
    nonce: &str,
    spent: (u64, u64),
    written_blobs: &mut Vec<std::path::PathBuf>,
) {
    let stem = format!("seamless-{}-{nonce}", std::process::id());
    let (mut aggregate_cells, spent_bytes) = spent;
    let mut held_bytes = 0_u64;
    for pane in held {
        let (local_id, cp) = (&pane.local_id, &pane.checkpoint);
        if manifest.held.len() >= MAX_HELD_PANES
            || manifest
                .sessions
                .iter()
                .any(|rec| rec.local_id == *local_id)
            || manifest.held.iter().any(|pane| pane.local_id == *local_id)
        {
            continue;
        }
        let meta_json = match held_screen_meta(cp) {
            Ok(meta_json) => meta_json,
            Err(why) => {
                aterm_log::warn!("update handoff: held pane {local_id} is not carried: {why}");
                continue;
            }
        };
        let meta = CheckpointMeta::from_checkpoint(cp);
        let bytes = pane.grid_bytes();
        let mut cells = aggregate_cells;
        if cp.history_lines != 0
            || (!cp.parser_ground && cp.parser.is_none())
            || admit_checkpoint_dimensions(&mut cells, cp.rows, cp.cols, 0, cp.alt_grid.is_some())
                .is_err()
            || checkpoint_grid_cap(&meta)
                .is_none_or(|cap| u64::try_from(cp.grid.len()).map_or(true, |len| len > cap))
            || !checkpoint_grid_is_canonical(&cp.grid, cp.rows, cp.cols, 0)
            || cp.alt_grid.is_some() != cp.alt_cursor.is_some()
            || cp.alt_grid.as_ref().is_some_and(|alt| {
                u64::try_from(alt.len())
                    .map_or(true, |len| len > checkpoint_grid_cap(&meta).unwrap_or(0))
                    || !checkpoint_grid_is_canonical(alt, cp.rows, cp.cols, 0)
            })
            || held_bytes.saturating_add(bytes) > MAX_HELD_AGGREGATE_GRID_BYTES
            || spent_bytes.saturating_add(held_bytes).saturating_add(bytes)
                > MAX_HANDOFF_AGGREGATE_GRID_BYTES
        {
            continue;
        }
        let grid_file = dir.join(held_sidecar_name(&stem, *local_id, false));
        if write_private(&grid_file, &cp.grid).is_err() {
            let _ = std::fs::remove_file(&grid_file);
            continue;
        }
        let alt_grid_file = match cp.alt_grid.as_ref() {
            Some(alt) => {
                let path = dir.join(held_sidecar_name(&stem, *local_id, true));
                if write_private(&path, alt).is_err() {
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(&grid_file);
                    continue;
                }
                Some(path)
            }
            None => None,
        };
        aggregate_cells = cells;
        held_bytes = held_bytes.saturating_add(bytes);
        let (exit_code, exit_signal) = match pane.exit {
            Some(aterm_pty::ChildExit::Code(code)) => (Some(code), None),
            Some(aterm_pty::ChildExit::Signal(signal)) => (None, Some(signal)),
            None => (None, None),
        };
        // Its scrollback: joined and named as a handed session's is, or left behind and counted
        // with why, so the successor reports a failed carry as a failure and a choice as one.
        let export = history
            .exports
            .iter()
            .position(|export| export.local_id == *local_id)
            .map(|at| history.exports.swap_remove(at));
        let carried = match pane.history_head {
            Some(head) => crate::handoff_history::name_held_sidecar(
                export,
                head,
                dir.join(held_history_name(&stem, *local_id)),
                history.unexported,
            ),
            None => crate::handoff_history::HeldHistory {
                stamp: None,
                dropped: pane.history_dropped,
                why: history.unexported,
                path: None,
            },
        };
        written_blobs.extend(carried.path);
        let left_behind = carried.dropped > 0;
        // Its turn ledger and archive tail, under a handed session's control-carry caps; one
        // that does not go is logged, and the successor's ledger names the turn ids it cannot
        // vouch for.
        let control = controls
            .iter()
            .find(|(id, _)| id == local_id)
            .and_then(|(_, bytes)| {
                let len = bytes.len() as u64;
                if len > crate::handoff_carry::MAX_SIDECAR_BYTES
                    || aggregate_control.saturating_add(len)
                        > crate::handoff_carry::MAX_AGGREGATE_BYTES
                {
                    aterm_log::warn!(
                        "update apply: held pane {local_id}'s control carry ({len} bytes) is past \
                         the sidecar or aggregate cap; it crosses without its turn ledger and \
                         archive tail"
                    );
                    return None;
                }
                let path = dir.join(held_control_name(&stem, *local_id));
                if write_private_new(&path, bytes).is_none() {
                    aterm_log::warn!(
                        "update apply: held pane {local_id}'s control carry could not be \
                         written; it crosses without its turn ledger and archive tail"
                    );
                    return None;
                }
                *aggregate_control = aggregate_control.saturating_add(len);
                written_blobs.push(path);
                Some(crate::handoff_carry::stamp(bytes))
            });
        manifest.held.push(crate::session_store::HeldPaneCarry {
            local_id: *local_id,
            exit_code,
            exit_signal,
            sid: Some(pane.sid.as_str().to_owned()),
            history_dropped: carried.dropped,
            history_withheld: left_behind
                && carried.why == crate::handoff_history::HeldLoss::Withheld,
            history_failed: left_behind && carried.why == crate::handoff_history::HeldLoss::Failed,
            history: carried.stamp,
            control,
            failure_seen: pane.failure_seen,
            screen: ScreenCarry {
                schema: ScreenCarry::SCHEMA,
                meta: meta_json,
                grid_file: grid_file.to_string_lossy().into_owned(),
                alt_grid_file: alt_grid_file
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                repaint: false,
            },
        });
        written_blobs.push(grid_file);
        written_blobs.extend(alt_grid_file);
    }
}

/// One held pane's final screen as a successor admitted it: what `App::restore_held_leaf` shows,
/// read-only, in the pane its layout leaf names.
pub(crate) struct HeldPane {
    pub local_id: u64,
    /// The session id the outgoing process knew the pane by
    /// ([`crate::session_store::HeldPaneCarry::sid`]), when this process may take it, so
    /// `aterm ctl @<sid>` still reaches the pane.
    pub sid: Option<SessionId>,
    pub checkpoint: TerminalCheckpoint,
    /// How its command ended, as the outgoing process carried it
    /// ([`crate::session_store::HeldPaneCarry::exit_code`]); `None` when it
    /// carried neither, or (malformed) both.
    pub exit: Option<aterm_pty::ChildExit>,
    /// Its scrollback: the sidecar its outgoing process named, opened before the proof, and the
    /// lines that do not cross, counted; imported after Commit (`App::restore_held_leaf`).
    pub history: crate::handoff_history::AdoptedHistory,
    /// Its turn ledger and archive tail, read before the proof; `None` when the outgoing process
    /// carried none, and the pane's ledger then names the turn ids it cannot vouch for
    /// (`handoff_carry::adopted_ledger`).
    pub control: Option<crate::handoff_carry::ControlCarry>,
    /// Whether the person had seen the failure its exit marks the tab for
    /// ([`crate::session_store::HeldPaneCarry::failure_seen`]); `false` from an older producer.
    pub failure_seen: bool,
}

/// Read every held pane the outgoing process named, after every handed session is admitted,
/// from what their reads left of the aggregate byte and cell caps (`remaining_bytes`,
/// `used_cells`) and within [`MAX_HELD_AGGREGATE_GRID_BYTES`] of their own. Total: a pane that
/// is not this build's to show (a handed id, a repeat, past [`MAX_HELD_PANES`], a meta that does
/// not parse or carries scrollback, a sidecar not its own or past a cap, a screen the content
/// predicates refuse) is left out with its sidecars removed, and the successor keeps its
/// placeholder; nothing here refuses the handoff. A pane shown has its scrollback sidecar opened
/// and unlinked here, before the proof, from `remaining_history`.
///
/// Held screens are outside every digest (an older successor, which skips `held`, could not
/// compute it), so a same-user process that swapped the sidecars could change what the pane
/// shows, and so it is shown as text only: the pane has no process or PTY, and everything that
/// could act is dropped here (every hyperlink, the working directory, the shell-integration
/// nonce, secure keyboard entry, the taskbar progress, any partial parser state).
///
/// Its session id goes on, so `@<sid>` still reaches the pane, but only a sid of the shape this
/// build mints and not one a handed session (`handed`: each one's id and sid) or an earlier held
/// pane answers to; otherwise the pane gets a fresh one, so no address names two sessions.
#[allow(clippy::too_many_arguments)]
fn take_held_panes(
    held: &[crate::session_store::HeldPaneCarry],
    handed: &[(u64, &str)],
    manifest_path: &std::path::Path,
    stem: &str,
    dir: &std::path::Path,
    remaining_bytes: &mut u64,
    used_cells: &mut u64,
    remaining_history: &mut u64,
    remaining_control: &mut u64,
) -> Vec<HeldPane> {
    let mut panes: Vec<HeldPane> = Vec::new();
    let mut held_budget = MAX_HELD_AGGREGATE_GRID_BYTES;
    for carry in held {
        let local_id = carry.local_id;
        let grid_path = manifest_path.with_file_name(held_sidecar_name(stem, local_id, false));
        let alt_path = manifest_path.with_file_name(held_sidecar_name(stem, local_id, true));
        let history_path = manifest_path.with_file_name(held_history_name(stem, local_id));
        let control_path = manifest_path.with_file_name(held_control_name(stem, local_id));
        let discard = || {
            let _ = std::fs::remove_file(&grid_path);
            let _ = std::fs::remove_file(&alt_path);
            let _ = std::fs::remove_file(&history_path);
            let _ = std::fs::remove_file(&control_path);
        };
        let meta = parse_checkpoint_meta(&carry.screen).filter(|meta| {
            meta.history_lines == 0 && carry.screen.meta.len() <= MAX_HELD_META_BYTES
        });
        if panes.len() >= MAX_HELD_PANES
            || handed.iter().any(|(id, _)| *id == local_id)
            || panes.iter().any(|pane| pane.local_id == local_id)
            || meta.is_none()
        {
            discard();
            continue;
        }
        let mut budget = held_budget.min(*remaining_bytes);
        let before = budget;
        let Ok(wire) = take_screen_wire(
            &carry.screen,
            meta.as_ref(),
            &grid_path,
            &alt_path,
            dir,
            &mut budget,
        ) else {
            discard();
            continue;
        };
        // What was read is spent, shown or not: it was allocated.
        let read = before.saturating_sub(budget);
        *remaining_bytes = remaining_bytes.saturating_sub(read);
        held_budget = held_budget.saturating_sub(read);
        // Cells are charged only for a screen shown: a refused one's blank
        // stand-in is not wanted here.
        let mut cells = *used_cells;
        let IncomingScreen::Exact(mut checkpoint) = admit_incoming_screen(meta, wire, &mut cells)
        else {
            discard();
            continue;
        };
        *used_cells = cells;
        let rows = usize::from(checkpoint.rows);
        let stripped = strip_every_link(&checkpoint.grid, rows).and_then(|grid| {
            match checkpoint.alt_grid.as_deref() {
                Some(alt) => strip_every_link(alt, rows).map(|alt| (grid, Some(alt))),
                None => Some((grid, None)),
            }
        });
        let Some((grid, alt_grid)) = stripped else {
            // An older producer writes grids past the strip bound; the pane then shows its
            // placeholder.
            aterm_log::warn!(
                "overlap handoff: held pane {local_id}'s screen was not shown: its grids are \
                 over the {MAX_LINK_STRIP_BYTES}-byte link-strip bound or do not decode; it \
                 keeps its placeholder"
            );
            discard();
            continue;
        };
        checkpoint.grid = grid;
        checkpoint.alt_grid = alt_grid;
        checkpoint.current_working_directory = None;
        checkpoint.shell_integration_nonce = None;
        checkpoint.secure_keyboard_entry = false;
        checkpoint.taskbar_progress = None;
        checkpoint.parser = None;
        checkpoint.parser_ground = true;
        let sid = carry
            .sid
            .as_deref()
            .filter(|sid| {
                crate::keeper_link::valid_sid(sid)
                    && !handed.iter().any(|(_, handed)| handed == sid)
                    && !panes.iter().any(|pane| {
                        pane.sid
                            .as_ref()
                            .is_some_and(|taken| taken.as_str() == *sid)
                    })
            })
            .map(SessionId::new);
        panes.push(HeldPane {
            local_id,
            sid,
            checkpoint,
            exit: match (carry.exit_code, carry.exit_signal) {
                (Some(code), None) => Some(aterm_pty::ChildExit::Code(code)),
                (None, Some(signal)) => Some(aterm_pty::ChildExit::Signal(signal)),
                _ => None,
            },
            // Its scrollback, opened and unlinked before the proof, as a handed session's is.
            history: crate::handoff_history::incoming_held(
                carry.history.as_deref(),
                carry.history_dropped,
                crate::handoff_history::HeldLoss::from_wire(
                    carry.history_withheld,
                    carry.history_failed,
                ),
                local_id,
                &history_path,
                dir,
                remaining_history,
            ),
            // Its turn ledger and archive tail, read before the proof from what the handed
            // sessions left of the control budget; a lost one is logged.
            control: carry.control.as_deref().and_then(|stamp| {
                take_control_sidecar(&control_path, dir, stamp, remaining_control)
                    .inspect_err(|cause| {
                        aterm_log::warn!(
                            "overlap handoff: held pane {local_id} crosses without its control \
                             carry (turn ledger, archive tail): {cause}"
                        );
                    })
                    .ok()
                    .map(|mut control| {
                        // Nothing draws in a held pane: the differ's state
                        // describes no screen this build will diff against.
                        control.without_differ();
                        control
                    })
            }),
            failure_seen: carry.failure_seen,
        });
    }
    panes
}

/// `blob` — `records` canonical line records — with every hyperlink dropped
/// and every glyph kept; `None` when it holds more than [`MAX_LINK_STRIP_BYTES`]
/// to decode or does not decode to exactly `records` lines.
fn strip_every_link(blob: &[u8], records: usize) -> Option<Vec<u8>> {
    if u64::try_from(blob.len()).map_or(true, |len| len > MAX_LINK_STRIP_BYTES) {
        return None;
    }
    let mut lines = aterm_core::scrollback::deserialize_lines(blob);
    if lines.len() != records {
        return None;
    }
    if !lines
        .iter()
        .any(aterm_core::scrollback::Line::has_hyperlinks)
    {
        return Some(blob.to_vec());
    }
    for line in &mut lines {
        line.clear_hyperlinks();
    }
    Some(aterm_core::scrollback::serialize_lines(&lines))
}

/// The physical artifacts one outgoing attempt published, plus the one screen commitment
/// computed over the exact bytes it wrote.
#[cfg(unix)]
pub(crate) struct OutgoingHandoff {
    pub manifest_path: String,
    pub nonce: String,
    pub fds_wire: String,
    /// [`screen_wire_digest`] over the carried meta/grid bytes as written. The worker asserts it
    /// equals the digest the main thread committed to before parking, so a difference is a
    /// preparation failure, not the child's unexplained `AdoptionMismatch`.
    pub screen_digest: [u8; 32],
    /// The carried connections this manifest left out to fit its cap
    /// ([`shed_connections_to_fit`]). Not audited here: until the Commit this process still holds
    /// them, and an attempt that stands down dissolves nothing. The Commit audits each as a
    /// `session_edge` drop (`App::audit_connections_left_out`), once.
    pub connections_left_out: Vec<ConnectionCarry>,
}

/// Write the outgoing handoff manifest (nonce-stamped, `0600`, in the `0700` control dir) and
/// return the [`OutgoingHandoff`] whose first three fields become the child's env, or
/// [`WriteOutgoingFailure::NoPrivateDir`] when there is no private control dir (the caller then
/// re-execs without the seamless env: a cold apply). Every fd in `fds` is a child-owned CLOEXEC
/// duplicate; the caller clears the flag only in that command's `pre_exec` closure.
///
/// `screens` carries each session's engine checkpoint by `local_id`: the scalar meta goes in the
/// matching manifest record, the grid blobs in nonce-stamped `0600` sidecars beside the
/// manifest. `window` is the outgoing window's frame. Screen carry is required: every
/// authenticated PTY needs one valid meta+grid checkpoint (and an alt-grid blob when its
/// checkpoint names one), and any omission fails the whole preparation, so a child can never
/// Commit a blank or incomplete engine.
///
/// `controls` are the sessions' control-carry sidecars (`crate::handoff_carry`, by `local_id`),
/// written as `seamless-<pid>-<nonce>.s<id>.ctl` and named in the record by
/// `control = "<len> <sha256hex>"`. Unlike the screen carry they are best-effort and outside
/// both proof digests: one that cannot be written is left out.
///
/// `nonce` is the attempt nonce ([`mint_outgoing_nonce`]), supplied by the caller so the
/// successor can be launched with the manifest's path before this runs; it is echoed in the
/// result and written as the manifest's first line.
///
/// `repaint` names the sessions carried with a screen that is not what the program drew
/// ([`CarryRung::needs_repaint`]); their `ScreenCarry::repaint` tells the successor to make the
/// program redraw. No digest covers the flag.
///
/// The worker calls [`write_outgoing_with_held`]; this spelling, with no held panes, is the
/// tests'.
#[cfg(all(test, unix))]
pub(crate) fn write_outgoing(
    manifest: &SessionHandoff,
    fds: &HandoffFds,
    screens: &[(u64, TerminalCheckpoint)],
    repaint: &[u64],
    window: Option<WindowCarry>,
    controls: &[(u64, Vec<u8>)],
    nonce: &str,
) -> Result<OutgoingHandoff, WriteOutgoingFailure> {
    write_outgoing_with_held(
        manifest,
        fds,
        screens,
        repaint,
        window,
        controls,
        &[],
        crate::handoff_history::HeldExports::default(),
        nonce,
    )
}

/// [`write_outgoing`] with the held panes' final screens: `held` names each pane kept open after
/// its command exited, by pool id, with its visible screen, and `held_history` their
/// scrollback's exports, each joined and named beside its screen. Best-effort and outside both
/// proof digests, like the control carry: a pane this writer would not carry, whose sidecars it
/// cannot write, or that does not fit the held budgets or the successor's manifest cap is left
/// out, and the successor shows its placeholder. Nothing here fails the preparation.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_outgoing_with_held(
    manifest: &SessionHandoff,
    fds: &HandoffFds,
    screens: &[(u64, TerminalCheckpoint)],
    repaint: &[u64],
    window: Option<WindowCarry>,
    controls: &[(u64, Vec<u8>)],
    held: &[HeldScreen],
    held_history: crate::handoff_history::HeldExports,
    nonce: &str,
) -> Result<OutgoingHandoff, WriteOutgoingFailure> {
    let dir = crate::control_auth::socket_dir().ok_or(WriteOutgoingFailure::NoPrivateDir)?;
    let path = dir.join(outgoing_manifest_name(nonce));

    let _ = validated_identities(manifest, fds).ok_or(WriteOutgoingFailure::Inconsistent)?;
    let mut session_ids = manifest
        .sessions
        .iter()
        .map(|record| record.local_id)
        .collect::<Vec<_>>();
    let mut screen_ids = screens.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let mut fd_ids = fds.entries.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
    session_ids.sort_unstable();
    screen_ids.sort_unstable();
    fd_ids.sort_unstable();
    if session_ids != screen_ids
        || session_ids != fd_ids
        || session_ids.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err(WriteOutgoingFailure::Inconsistent);
    }

    // Attach the screen carries all-or-nothing. Track each blob so a partial
    // physical write is retired before returning (the caller files ProducerFailed).
    let mut manifest = manifest.clone();
    manifest.window = window;
    let mut written_blobs = Vec::with_capacity(screens.len().saturating_mul(2));
    let mut carried_metas: Vec<(u64, String)> = Vec::with_capacity(screens.len());
    let mut aggregate_cells = 0_u64;
    let mut aggregate_bytes = 0_u64;
    for (local_id, cp) in screens {
        let Some(rec) = manifest
            .sessions
            .iter_mut()
            .find(|r| r.local_id == *local_id)
        else {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return Err(WriteOutgoingFailure::Inconsistent);
        };
        let checkpoint_meta = CheckpointMeta::from_checkpoint(cp);
        let admitted_cap = admit_checkpoint_dimensions(
            &mut aggregate_cells,
            cp.rows,
            cp.cols,
            cp.history_lines,
            cp.alt_grid.is_some(),
        );
        if (!cp.parser_ground && cp.parser.is_none())
            || admitted_cap.is_err()
            || checkpoint_grid_cap(&checkpoint_meta)
                .is_none_or(|cap| u64::try_from(cp.grid.len()).map_or(true, |len| len > cap))
            || !checkpoint_grid_is_canonical(&cp.grid, cp.rows, cp.cols, cp.history_lines)
            || cp.alt_grid.is_some() != cp.alt_cursor.is_some()
            || cp.alt_grid.as_ref().is_some_and(|alt| {
                u64::try_from(alt.len()).map_or(true, |len| {
                    len > checkpoint_grid_cap(&checkpoint_meta).unwrap_or(0)
                }) || !checkpoint_grid_is_canonical(alt, cp.rows, cp.cols, 0)
            })
        {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return Err(WriteOutgoingFailure::Inconsistent);
        }
        aggregate_bytes = u64::try_from(cp.grid.len())
            .ok()
            .and_then(|len| aggregate_bytes.checked_add(len))
            .and_then(|total| {
                cp.alt_grid.as_ref().map_or(Some(total), |alt| {
                    u64::try_from(alt.len())
                        .ok()
                        .and_then(|len| total.checked_add(len))
                })
            })
            .unwrap_or(u64::MAX);
        if aggregate_bytes > MAX_HANDOFF_AGGREGATE_GRID_BYTES {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return Err(WriteOutgoingFailure::Inconsistent);
        }
        let Ok(meta) = aterm_json::to_string(&checkpoint_meta) else {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return Err(WriteOutgoingFailure::Inconsistent);
        };
        let grid_file = dir.join(format!(
            "seamless-{}-{nonce}.s{local_id}.grid",
            std::process::id()
        ));
        if let Err(error) = write_private(&grid_file, &cp.grid) {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return Err(WriteOutgoingFailure::Io(error.kind()));
        }
        written_blobs.push(grid_file.clone());
        let alt_grid_file = if let Some(alt) = cp.alt_grid.as_ref() {
            let p = dir.join(format!(
                "seamless-{}-{nonce}.s{local_id}.altgrid",
                std::process::id()
            ));
            if let Err(error) = write_private(&p, alt) {
                for path in written_blobs {
                    let _ = std::fs::remove_file(path);
                }
                return Err(WriteOutgoingFailure::Io(error.kind()));
            }
            written_blobs.push(p.clone());
            Some(p.to_string_lossy().into_owned())
        } else {
            None
        };
        carried_metas.push((*local_id, meta.clone()));
        rec.screen = Some(ScreenCarry {
            schema: ScreenCarry::SCHEMA,
            meta,
            grid_file: grid_file.to_string_lossy().into_owned(),
            alt_grid_file,
            repaint: repaint.contains(local_id),
        });
    }
    // The commitment, over the bytes published above (the same meta strings and grid blobs).
    // The child recomputes it from the bytes it reads back: one byte string, one function.
    let mut wire_entries = Vec::with_capacity(screens.len());
    for (local_id, cp) in screens {
        let Some((_, meta)) = carried_metas.iter().find(|(id, _)| id == local_id) else {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return Err(WriteOutgoingFailure::Inconsistent);
        };
        wire_entries.push(ScreenWireEntry {
            local_id: *local_id,
            meta: meta.as_bytes(),
            grid: &cp.grid,
            alt_grid: cp.alt_grid.as_deref(),
        });
    }
    let Some(carry_digest) = screen_wire_digest(&mut wire_entries) else {
        for path in written_blobs {
            let _ = std::fs::remove_file(path);
        }
        return Err(WriteOutgoingFailure::Inconsistent);
    };
    drop(wire_entries);
    // The control carry, after the commitment: it is not in it. Each sidecar joins
    // `written_blobs`, so every failure below retires it with the grids. A sidecar that does
    // not go is logged; the session adopts with an empty ledger and a fresh archive.
    let mut aggregate_control = 0_u64;
    for (local_id, bytes) in controls {
        // A held pane's carry is named with its screen (`stamp_held_panes`).
        let Some(rec) = manifest
            .sessions
            .iter_mut()
            .find(|r| r.local_id == *local_id)
        else {
            continue;
        };
        let len = bytes.len() as u64;
        if len > crate::handoff_carry::MAX_SIDECAR_BYTES
            || aggregate_control + len > crate::handoff_carry::MAX_AGGREGATE_BYTES
        {
            aterm_log::warn!(
                "update apply: session {local_id}'s control carry ({len} bytes) is past the \
                 sidecar or aggregate cap; it hands off without its turn ledger and archive tail"
            );
            continue;
        }
        let path = dir.join(format!(
            "seamless-{}-{nonce}.s{local_id}.ctl",
            std::process::id()
        ));
        if write_private_new(&path, bytes).is_some() {
            aggregate_control += len;
            written_blobs.push(path);
            rec.control = Some(crate::handoff_carry::stamp(bytes));
        } else {
            aterm_log::warn!(
                "update apply: session {local_id}'s control carry could not be written; it hands \
                 off without its turn ledger and archive tail"
            );
        }
    }
    // The held panes, after the commitment: they are not in it. Their sidecars join
    // `written_blobs`, so every failure below retires them with the grids.
    let mut held_history = held_history;
    stamp_held_panes(
        &mut manifest,
        held,
        &mut held_history,
        controls,
        &mut aggregate_control,
        &dir,
        nonce,
        (aggregate_cells, aggregate_bytes),
        &mut written_blobs,
    );
    // The successor refuses a manifest past this cap whole. Three parts are optional and give
    // way, until it fits, in the order that loses least for good:
    //
    // * The connections, whenever leaving them out alone brings the manifest under: a
    //   connection not carried fails closed (the successor's controller holds less authority)
    //   and is made again with `connect`, where a held pane's screen and a queued post are lost
    //   for good. The sources holding the most rows go first, only as many as cover the excess.
    //   Whether that suffices is measured (the manifest with no connection row against the
    //   cap), never estimated from the per-row bound, which counts framing high.
    // * Otherwise the held panes, last first, with their sidecars; their placeholders show.
    // * Then the mailbox: the largest session's carry sheds its rows (each post retired dead
    //   `not-carried` in the successor, each receipt counted), keeping its id counters.
    //
    // After each piece of content goes the connections are asked first again, so content that
    // had to go anyway goes before them. With nothing optional left, the manifest is refused
    // here, typed, rather than written past the cap.
    let mut connections_left_out = Vec::new();
    let manifest_bytes =
        |toml: &str| u64::try_from(nonce.len() + 1 + toml.len()).unwrap_or(u64::MAX);
    // The file's size with no connection row, as written: whether leaving connections out
    // alone brings it under the cap. It changes only when content goes.
    let mut without_connections: Option<u64> = None;
    let toml = loop {
        let Some(toml) = manifest.to_toml().ok() else {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return Err(WriteOutgoingFailure::Inconsistent);
        };
        let size = manifest_bytes(&toml);
        if size <= MAX_HANDOFF_MANIFEST_BYTES {
            break toml;
        }
        let excess = size.saturating_sub(MAX_HANDOFF_MANIFEST_BYTES);
        if !manifest.connections.is_empty() {
            let bare = match without_connections {
                Some(bare) => Some(bare),
                None => {
                    let rows = std::mem::take(&mut manifest.connections);
                    let bare = manifest.to_toml().ok().map(|toml| manifest_bytes(&toml));
                    manifest.connections = rows;
                    without_connections = bare;
                    bare
                }
            };
            let Some(bare) = bare else {
                for path in written_blobs {
                    let _ = std::fs::remove_file(path);
                }
                return Err(WriteOutgoingFailure::Inconsistent);
            };
            if bare <= MAX_HANDOFF_MANIFEST_BYTES {
                connections_left_out
                    .extend(shed_connections_to_fit(&mut manifest.connections, excess));
                continue;
            }
        }
        if let Some(dropped) = manifest.held.pop() {
            without_connections = None;
            for sidecar in std::iter::once(dropped.screen.grid_file.as_str())
                .chain(dropped.screen.alt_grid_file.as_deref())
            {
                let _ = std::fs::remove_file(sidecar);
            }
            let stem = format!("seamless-{}-{nonce}", std::process::id());
            if dropped.history.is_some() {
                let _ = std::fs::remove_file(dir.join(held_history_name(&stem, dropped.local_id)));
            }
            if dropped.control.is_some() {
                let _ = std::fs::remove_file(dir.join(held_control_name(&stem, dropped.local_id)));
            }
            aterm_log::info!(
                "overlap handoff: held pane {} left out of the manifest to keep it under the \
                 {MAX_HANDOFF_MANIFEST_BYTES}-byte cap; the successor shows its placeholder",
                dropped.local_id
            );
            continue;
        }
        if let Some(rec) = manifest
            .sessions
            .iter_mut()
            .filter(|r| r.fabric.rows_cost() > 0)
            .max_by_key(|r| r.fabric.rows_cost())
        {
            let posts = rec.fabric.posts.len();
            let receipts = rec.fabric.receipts.len() + rec.fabric.earlier_receipts.len();
            rec.fabric.shed();
            aterm_log::warn!(
                "overlap handoff: session {} carries {posts} waiting post(s) and {receipts} owed \
                 receipt(s) no further, to keep the manifest under the \
                 {MAX_HANDOFF_MANIFEST_BYTES}-byte cap; the successor retires the posts dead \
                 (`not-carried`)",
                rec.local_id
            );
            without_connections = None;
            continue;
        }
        for path in written_blobs {
            let _ = std::fs::remove_file(path);
        }
        return Err(WriteOutgoingFailure::ManifestOverCap);
    };
    // First line is the nonce (bound to the env copy); the rest is the manifest TOML.
    let body = format!("{nonce}\n{toml}");
    // 0600: readable only by us (the dir is already 0700 per-user).
    if let Err(error) = write_private(&path, body.as_bytes()) {
        for path in written_blobs {
            let _ = std::fs::remove_file(path);
        }
        return Err(WriteOutgoingFailure::Io(error.kind()));
    }
    Ok(OutgoingHandoff {
        manifest_path: path.to_string_lossy().into_owned(),
        nonce: nonce.to_string(),
        fds_wire: fds.encode(),
        screen_digest: carry_digest,
        connections_left_out,
    })
}

/// What one carried connection row costs the manifest past its three strings:
/// the `[[connections]]` header, the three keys, their quotes and newlines — an
/// upper bound on what the TOML writer adds (it writes 43 bytes today).
#[cfg(unix)]
const CONNECTION_ROW_OVERHEAD: u64 = 64;

/// What one carried connection row costs the manifest, at most: its three strings as a TOML
/// basic string escapes them (bounded as a meta's is), plus [`CONNECTION_ROW_OVERHEAD`]. It only
/// ranks the sources and sizes one pass of [`shed_connections_to_fit`]: an over-count leaves out
/// fewer rows than the excess needs, never more, and the writer's next measured pass leaves out
/// the next source.
#[cfg(unix)]
fn connection_row_cost(row: &ConnectionCarry) -> u64 {
    [&row.src, &row.dst, &row.op]
        .into_iter()
        .map(|text| meta_json_manifest_cost(text.as_bytes()))
        .fold(CONNECTION_ROW_OVERHEAD, u64::saturating_add)
}

/// Leave out the carried connections of the sources that hold the most of them, one whole
/// source at a time (at least one), until their rows' manifest cost ([`connection_row_cost`], an
/// upper bound) reaches `excess` bytes, and return the rows left out. A source's rows go
/// together, so no connection is carried as a fragment the successor would re-mint as a lesser
/// kind. The count is logged; nothing is audited here, since this process holds every row until
/// its Commit and an attempt that stands down dissolves none
/// ([`OutgoingHandoff::connections_left_out`]).
#[cfg(unix)]
fn shed_connections_to_fit(
    connections: &mut Vec<ConnectionCarry>,
    excess: u64,
) -> Vec<ConnectionCarry> {
    let mut by_source: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    for row in connections.iter() {
        let cost = by_source.entry(row.src.as_str()).or_default();
        *cost = cost.saturating_add(connection_row_cost(row));
    }
    let mut ranked = by_source.into_iter().collect::<Vec<_>>();
    // The costliest source first; ties in source order, so the choice is stable.
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mut freed = 0_u64;
    let mut shed = std::collections::BTreeSet::new();
    for (source, cost) in ranked {
        if freed >= excess && !shed.is_empty() {
            break;
        }
        freed = freed.saturating_add(cost);
        shed.insert(source.to_string());
    }
    let (left_out, kept): (Vec<_>, Vec<_>) = std::mem::take(connections)
        .into_iter()
        .partition(|row| shed.contains(&row.src));
    *connections = kept;
    aterm_log::warn!(
        "overlap handoff: {} connection row(s) of {} source session(s) left out of the \
         manifest to keep it under the {MAX_HANDOFF_MANIFEST_BYTES}-byte cap; if this attempt \
         commits, the successor's sessions hold less authority (each row audited as dropped \
         at the Commit), and each connection is made again with `connect`",
        left_out.len(),
        shed.len()
    );
    left_out
}

/// The consumed incoming handoff: the adopted live sessions plus the outgoing
/// window's frame carry. Empty `adopted` ⇒ no seamless handoff (or it failed
/// auth): start fresh.
#[derive(Default)]
pub(crate) struct IncomingHandoff {
    pub adopted: Vec<Adopted>,
    pub window: Option<WindowCarry>,
    /// Authenticated single-use nonce retained only long enough to bind the
    /// child's eventual adoption proof to this exact parent attempt.
    pub nonce: Option<String>,
    /// Attempt-bound layout sidecar consumed from the same private nonce prefix. `None` beside a
    /// non-empty `adopted` when the sidecar was read and hashed but did not parse or did not name
    /// exactly the adopted sessions: the orphan net then places the shells as tabs.
    pub layout: Option<crate::restore::RestoreManifest>,
    /// [`layout_wire_digest`] over the exact sidecar bytes this process read, never a
    /// re-serialization of `layout`: the outgoing process hashed the same bytes, so this matches
    /// across a version boundary. `None` when no sidecar was read.
    pub layout_digest: Option<[u8; 32]>,
    /// [`screen_wire_digest`] over the exact carried meta/grid bytes, for the same reason.
    pub screen_digest: Option<[u8; 32]>,
    /// The manifest's tokenless connection triples, verbatim. Parked on the `App` and re-minted
    /// through the one kind-bounded helper once every adopted session is registered: never a
    /// token or nonce, so nothing here is authority until the re-mint.
    pub connections: Vec<ConnectionCarry>,
    /// The unsaved Settings drafts a layout carried that this build could not place (`layout` is
    /// `None`), with how many its parse dropped. The drafts ride only in the layout, so the
    /// restore reports them with their text (`App::settle_carried_settings_drafts`).
    pub unplaced_settings_drafts: (Vec<crate::restore::SettingsDraftRestore>, usize),
    /// The final screens of the panes the outgoing process kept open after their commands
    /// exited, each read-only, matched to its layout leaf by the placeholder that stands for it
    /// (`PlaceholderLeafRestore::unhanded`). Never adopted, never in the proof.
    pub held: Vec<HeldPane>,
    /// Whose content a whole-handoff refusal was about ([`refused_whole`]); `None` for an
    /// admitted handoff and for a boot offered none. Decides how the refusing successor leaves
    /// ([`refused_intake_leave`]).
    #[cfg_attr(not(unix), expect(dead_code, reason = "read only by the unix boot"))]
    pub refusal: Option<WholeRefusal>,
    /// The outgoing store's exit-ledger sequence
    /// ([`crate::session_store::SessionHandoff::roster_seq`]), which this process's store
    /// continues above before it registers a session. `None` from an older producer, and for a
    /// boot offered no handoff.
    pub roster_seq: Option<u64>,
    /// The argv the outgoing process's bridge was attached to at runtime
    /// ([`crate::session_store::SessionHandoff::fabric_attached`]), empty when none; this
    /// process's supervisor arms it when it has no command configured.
    #[cfg_attr(not(unix), expect(dead_code, reason = "read only by the unix boot"))]
    pub fabric_attached: Vec<String>,
    /// What the outgoing status item knew of each session, by sid
    /// ([`crate::session_store::SessionRecord::status_seen`]), as the record
    /// carried it: read at each adopted session's first publication
    /// (`App::adopt_status_carry`), never a reason to refuse. Empty for a
    /// boot that was offered no handoff.
    pub status_seen: Vec<(String, String)>,
    /// Each adopted session's Autopilot end and its task's work, by sid
    /// ([`crate::session_store::SessionRecord::autopilot_end`], read by
    /// [`end_carry_of`]): the successor's `App::end_carry` as it is, spent
    /// at each adopted session's first publication (`App::adopt_end_carry`),
    /// never a reason to refuse. Empty for a boot that was offered no
    /// handoff.
    pub end_carry: std::collections::HashMap<String, crate::harness_end_carry::EndCarry>,
    /// The fleet needs the outgoing herald had told
    /// ([`crate::session_store::SessionHandoff::fleet_told`]), as the
    /// manifest carried them: read at this process's first fresh fleet read
    /// (`App::herald_fleet`), never a reason to refuse. Empty for a boot
    /// that was offered no handoff.
    pub fleet_told: Vec<String>,
    /// Where the outgoing process will name, at its Commit, how long ago a
    /// person last keyed each carried session ([`person_stamps_path`] of the
    /// manifest this process read), for the Commit to take
    /// (`App::take_carried_person_stamps`). `None` for a boot that adopted
    /// nothing.
    #[cfg(unix)]
    pub person_stamps: Option<std::path::PathBuf>,
    /// The outgoing process's bus-mirror tombstones
    /// ([`crate::session_store::SessionHandoff::mirror_dropped`]), the rows as
    /// written; adopted into this process's store before its first session.
    pub mirror_dropped: Vec<String>,
    /// Whether the outgoing process's node mirrored
    /// ([`crate::session_store::SessionHandoff::mirror_served`]).
    pub mirror_served: bool,
}

/// Whose content a whole-handoff refusal was about. The successor's exit status is all the
/// parked parent reads of it, and an exit other than [`EXIT_SUCCESSOR_PASSING`] is filed
/// structural against the candidate, which is right only for this build's own verdict. 75 is
/// read as meant only by a parent that declared it ([`ENV_EXIT_CAPS`]); every other shipped
/// parent files it structural too ([`refused_intake_leave`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WholeRefusal {
    /// The outgoing build's artifacts: an environment that arrived in part, a descriptor list, a
    /// manifest or layout sidecar that could not be read (over the size cap: parents through
    /// v0.98.0 wrote past 4 MiB for a shell-heavy desk, which this build reads up to
    /// [`MAX_INCOMING_MANIFEST_BYTES`]), a nonce naming another attempt, a path outside the
    /// control directory. Nothing about the candidate's bytes, so filing it structural would
    /// blame the new build for the old build's content (L1).
    ///
    /// Also every per-session hard refusal and the descriptor bijection: a descriptor list that
    /// does not match the manifest's sessions, a master that is not a tty, a record with no
    /// screen, a grid sidecar misnamed, missing or past a byte cap, and a pool the proof cannot
    /// frame. Each is what the outgoing build wrote.
    Producer,
    /// This build's verdict on content it did read: a manifest it cannot parse (the consumer is
    /// only ever more lenient, L3, so that is this build's to answer for), or its own adopted set
    /// disagreeing with itself.
    Own,
}

impl IncomingHandoff {
    /// How many adopted sessions arrive on a blank screen to be repainted, whichever side
    /// degraded them: the outgoing capture carried the session below its exact rungs
    /// (`ScreenCarry::repaint`), or this build refused the carried screen's content
    /// ([`admit_incoming_screen`]). The boot keeps it for the landing record
    /// ([`crate::update_words::landed`]), which names it, so a blank tab is never unexplained.
    /// The unit is the session (a split pane is its own); the record says "tab".
    #[must_use]
    pub(crate) fn repainted_tabs(&self) -> usize {
        self.adopted
            .iter()
            .filter(|adopted| adopted.repaint)
            .count()
    }
}

/// Every master the incoming wire names, owned from the moment it is read until
/// an adoption claims it: whatever is still here when [`take_incoming_from`] returns —
/// the whole set on a refusal, anything no authenticated session named on success
/// — is closed. Together with [`crate::spawn::HandedMaster`], which owns a claimed
/// master until its session's sink does, a handed-off master always has exactly
/// one owner, so it is adopted or closed and never both.
#[cfg(unix)]
struct IncomingPtyGuard(Vec<i32>);

#[cfg(unix)]
impl IncomingPtyGuard {
    fn from_wire(wire: &str) -> Self {
        let mut fds = wire
            .split(',')
            .filter_map(parse_named_fd)
            .filter(|fd| *fd >= 3)
            .collect::<Vec<_>>();
        fds.sort_unstable();
        fds.dedup();
        Self(fds)
    }

    /// Move ownership of `fd` from this guard to the adoption being built.
    fn claim(&mut self, fd: i32) -> crate::spawn::HandedMaster {
        self.0.retain(|owned| *owned != fd);
        crate::spawn::HandedMaster::new(fd)
    }
}

#[cfg(unix)]
impl Drop for IncomingPtyGuard {
    fn drop(&mut self) {
        for fd in self.0.drain(..) {
            aterm_pty::close_fd(fd);
        }
    }
}

/// Overlap-handoff rollback: delete every artifact a [`write_outgoing`] with this `nonce`
/// produced (all named `seamless-<pid>-<nonce>…` in our 0700 control dir, `.ctl` sidecars
/// included). Under the overlap a child that died before consuming them leaves them on disk, and
/// the still-running parent retires them (each retry mints a fresh nonce). Best-effort and bound
/// to our own pid and the exact nonce; a crashed sender's leftovers are
/// [`sweep_dead_handoff_leftovers`]'s.
#[cfg(any(unix, test))]
pub(crate) fn discard_outgoing(nonce: &str) {
    let Some(dir) = crate::control_auth::socket_dir() else {
        return;
    };
    discard_outgoing_in(&dir, nonce);
}

/// [`discard_outgoing`] in `dir`, the control directory it resolved. It also retires a layout
/// sidecar the artifact diet wrote before the park (`handoff_diet::speculate_layout`), which
/// carries this attempt's prefix (`NothingSpeculativeOutlivesTheAttempt`).
#[cfg(any(unix, test))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_speculative_sidecar",
        action = "Retire",
        project = "aterm_gui::handoff_diet::conformance::project"
    )
)]
pub(crate) fn discard_outgoing_in(dir: &std::path::Path, nonce: &str) {
    let prefix = format!("seamless-{}-{nonce}", std::process::id());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Unlink this attempt's control-carry, history-carry and held-grid sidecars once the
/// successor's proof has checked out. A receiver that reads them deletes them as it does; one
/// built before those carries never looks at them, and nothing else would remove the turn text
/// and scrolled-off rows they hold. Prefix- and suffix-bound to our own pid and this nonce.
///
/// This relies on the receiver reading every control sidecar and opening every history sidecar
/// before it publishes its proof (`take_incoming` does, single-threaded, ahead of
/// `take_ready_fd`): an open descriptor outlives the unlink, so the history is read after
/// Commit.
#[cfg(unix)]
pub(crate) fn retire_outgoing_controls(nonce: &str) {
    let Some(dir) = crate::control_auth::socket_dir() else {
        return;
    };
    let prefix = format!("seamless-{}-{nonce}.", std::process::id());
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix)
            && [".ctl", ".hist", HELD_GRID_SUFFIX, HELD_ALT_SUFFIX]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Remove the handoff files (`seamless-<pid>-…`: manifests, layouts, grids, control sidecars) a
/// dead process left in the control dir, and the restore machinery's hidden siblings of its
/// layout (`.seamless-<pid>-….aterm-restore.lock` and the like, [`crate::restore`]). A sender
/// that crashed before its rollback or a successor's read leaves screens, turn text and
/// scrolled-off rows on disk. Run at startup and after a committed handoff
/// (`control::sweep_after_handoff`). A live pid's files (ours, or a running handoff's) are never
/// touched; a reused pid only keeps a dead process's files a while longer.
pub(crate) fn sweep_dead_handoff_leftovers() {
    if let Some(dir) = crate::control_auth::socket_dir() {
        sweep_dead_handoff_leftovers_in(&dir, &crate::control_auth::pid_alive);
    }
}

fn sweep_dead_handoff_leftovers_in(dir: &std::path::Path, alive: &dyn Fn(u32) -> bool) {
    let own = std::process::id();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(handoff_leftover_pid) else {
            continue;
        };
        if pid == own || alive(pid) || e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let _ = std::fs::remove_file(e.path());
    }
}

/// The `<pid>` of a `seamless-<pid>-<rest>` file name, or of the restore
/// machinery's hidden `.seamless-<pid>-<rest>` sibling of one.
fn handoff_leftover_pid(name: &str) -> Option<u32> {
    let name = name.strip_prefix('.').unwrap_or(name);
    let (pid, rest) = name.strip_prefix("seamless-")?.split_once('-')?;
    (!pid.is_empty() && !rest.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        .then(|| pid.parse().ok())
        .flatten()
}

/// The overlap-handoff readiness pipe's write fd, passed by the parked parent
/// (`apply_staged_update_now`) so this process can say every carried window is painted. Its own
/// env var, not the `ATERM_SEAMLESS_FDS` wire: it is not a session, and it must fail the
/// `fd_is_tty` backstop.
pub(crate) const ENV_READY_FD: HandoffName = HandoffName::new("ATERM_HANDOFF_READY_FD");
pub(crate) const ENV_COMMIT_FD: HandoffName = HandoffName::new("ATERM_HANDOFF_COMMIT_FD");
/// Out-of-band lane: the single-use rendezvous socket the outgoing process bound before it
/// launched this successor. Its presence is the lane, with no version byte: only a parent with
/// out-of-band transport code can publish it, so no parent in the field is shown the new shape.
#[cfg(unix)]
pub(crate) const ENV_RENDEZVOUS: HandoffName = HandoffName::new("ATERM_HANDOFF_RENDEZVOUS");
/// The secret that admits exactly one dialer to that rendezvous.
#[cfg(unix)]
pub(crate) const ENV_CLAIM: HandoffName = HandoffName::new("ATERM_HANDOFF_CLAIM");
#[cfg(unix)]
pub(crate) const ENV_PARENT_PID: HandoffName = HandoffName::new("ATERM_HANDOFF_PARENT_PID");
/// The outgoing process's kernel birth record, published beside its pid. A pid alone is
/// recyclable; this makes it an identity the successor can verify without being the parent's
/// fork child (see [`AttestedParent`]).
#[cfg(unix)]
pub(crate) const ENV_PARENT_BIRTH: HandoffName = HandoffName::new("ATERM_HANDOFF_PARENT_BIRTH");
/// What the outgoing process can read of its successor's exit: a comma-separated capability
/// list, published beside the parent's identity ([`outgoing_parent_env`], so both lanes carry
/// it) and taken out of the snapshot by the successor before any shell exists
/// ([`take_parent_reads_passing_exit_from`]). A status is a message only to a parent that reads
/// it: every parent through 0.100.0 files every exit code structural and keeps the counted trial
/// launch, and the first parent that reads [`EXIT_SUCCESSOR_PASSING`] as passing also writes
/// this variable, so it is a successor's only proof its 75 will be read as meant.
#[cfg(unix)]
pub(crate) const ENV_EXIT_CAPS: HandoffName = HandoffName::new("ATERM_HANDOFF_EXIT_CAPS");
/// The one capability [`ENV_EXIT_CAPS`] names: this parent files a successor's
/// [`EXIT_SUCCESSOR_PASSING`] transient and gives its counted trial launch back
/// (`app_native::PhysicalFailureShape::of_child_death`).
#[cfg(unix)]
pub(crate) const EXIT_CAPS_PASSING: &str = "passing75";

/// The kernel's own birth record for a process: the microsecond-resolution
/// instant `fork` created it. Assigned by the kernel, so no process can choose
/// its own; and two processes that reuse a pid cannot share it, which is
/// exactly the property a bare pid lacks. Compared for EQUALITY only — it is an
/// identity token, never a clock reading.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessBirth {
    /// Start time, unix seconds.
    pub(crate) seconds: u64,
    microseconds: u64,
}

#[cfg(unix)]
impl ProcessBirth {
    fn to_wire(self) -> String {
        format!("{}.{}", self.seconds, self.microseconds)
    }

    fn from_wire(raw: &str) -> Option<Self> {
        let (seconds, microseconds) = raw.trim().split_once('.')?;
        Some(Self {
            seconds: seconds.parse().ok()?,
            microseconds: microseconds.parse().ok()?,
        })
    }
}

/// Read the kernel's record for `pid`, or `None` when there is no process we may treat as a
/// handoff parent there. `None` covers four facts, since the caller refuses on each:
///
/// * no such process (the parent exited and was reaped);
/// * a zombie, treated as dead so this probe's edge sits near the predicate it replaces
///   (`getppid() == pid`). On Darwin this probe goes false first and a child's `getppid()`
///   names the exiting parent for about a millisecond more (`tests::commit_parent_death_helper`);
///   the protocol needs neither order, since liveness reads one witness and the watch's last
///   poll lets a written Commit win;
/// * a process owned by another uid: every other authority in the overlap protocol is
///   uid-bounded (the 0700 control dir, `control_auth::peer_check`);
/// * the kernel disagreeing about the pid it just reported on.
///
/// The Claude Code footer's pid-reuse guard (`crate::claude_footer`) wants the same refusals.
#[cfg(target_os = "macos")]
pub(crate) fn read_process_birth(pid: libc::pid_t) -> Option<ProcessBirth> {
    // The kernel read lives in `aterm_uds::process`, shared with the PTY keeper.
    let birth = aterm_uds::process::read_process_birth(pid)?;
    Some(ProcessBirth {
        seconds: birth.seconds,
        microseconds: birth.microseconds,
    })
}

/// Other unixes have no birth-record primitive wired here, so they attest only through the
/// kernel parent link. That is sound because off macOS the successor is always a fork child
/// (`app_update_handoff::run_handoff_worker` has no other launch shape). A non-fork transport
/// off macOS needs a real implementation first (Linux's start record is boot-clock,
/// `aterm_uds::process::read_process_start`), or [`attest_handoff_parent`] has no witness.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn read_process_birth(_pid: libc::pid_t) -> Option<ProcessBirth> {
    None
}

/// This process's own birth record, for publication to a successor.
#[cfg(unix)]
#[must_use]
fn own_process_birth() -> Option<ProcessBirth> {
    read_process_birth(libc::pid_t::try_from(std::process::id()).ok()?)
}

/// How the successor learned which process its handoff parent is: the two mutually exclusive
/// situations a successor can be born into, each with the strongest identity primitive it
/// offers.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentWitness {
    /// The kernel's birth record for the attested pid, verified at admission. Independent of the
    /// process tree, so it answers for a successor launchd owns.
    Birth(ProcessBirth),
    /// The kernel parent link. It carries nothing once `getppid()` is 1, so it is chosen only
    /// while the attested pid is our live creator and libproc produced no birth record, so a
    /// machine without the liveness probe still updates.
    ForkLink,
}

/// Parental attestation: the one witness the overlap protocol trusts about who the outgoing
/// process is and whether it is still alive. The two halves fail in opposite directions:
///
/// 1. IDENTITY — established ONCE, at admission. [`ENV_PARENT_PID`] is an
///    environment variable, so anything that can exec this binary can write any
///    number into it. Admission must therefore make the claim agree with a fact
///    the writer of an environment does not control, or a stale `ATERM_HANDOFF_*`
///    block that leaked into a user shell (the case [`clear_handoff_env`] exists
///    to prevent, and this is its backstop) could point a fresh instance at some
///    unrelated live process. That instance would then treat a recognizable
///    handoff as VALID and carry the stale wire's descriptor numbers — which in
///    the new process name unrelated resources — through to the updater's exec.
///
/// `getppid() == pid` gives both, but only to a fork child: the parent link is kernel state no
/// environment can forge, and it flips inside the parent's `exit(2)` (before reap, so before the
/// pid can recycle) to 1 for good. A successor launched through LaunchServices has ppid 1 from
/// birth (the fork-lane `spawn` in `app_update_handoff::run_handoff_worker` says why that lane
/// exists), so the two properties are split:
///
/// `getppid() == pid` used to provide both at once, which is why it read as a
/// single cheap check: the parent link is kernel state no environment can
/// forge (1), it flips inside the parent's `exit(2)` — at exit, NOT at reap, so
/// strictly before the pid can be recycled — and on macOS its only replacement
/// value is 1, permanently (2). But it provides both ONLY to a fork child.
///
/// B1 (closed by this function; see "WHY THIS `spawn` IS STILL HERE" at the
/// `spawn` in `app_update_handoff::run_handoff_worker`): a successor launched
/// through LaunchServices has ppid 1 from birth, so `getppid()` cannot name its
/// parent, and the old predicate refused it at admission and then fail-stopped
/// it. The repair is to stop conflating the two properties:
///
/// `creator` is the kernel parent link, taken by the caller so both arms are testable without a
/// ppid-1 process; `creator_after` re-reads it once the birth record is in hand (a test stub can
/// simulate the parent exiting between the reads).
#[cfg(unix)]
#[must_use]
fn attest_handoff_parent_from(
    pid: libc::pid_t,
    published: Option<ProcessBirth>,
    creator: libc::pid_t,
    creator_after: impl FnOnce() -> libc::pid_t,
) -> Option<AttestedParent> {
    // pid 1 is never a real handoff parent: it is what reparenting produces.
    if pid <= 1 {
        return None;
    }
    if creator > 1 && creator != pid {
        // A live creator that is not who the environment named: no published detail overrides
        // the kernel. This arm refuses a stale handoff block re-entering through a shell.
        return None;
    }
    let live = read_process_birth(pid);
    let witness = match (published, live) {
        // The outgoing process named its own birth instant and the kernel agrees: sound
        // whether or not we are its child.
        (Some(published), Some(live)) if published == live => ParentWitness::Birth(live),
        // A claim the kernel does not corroborate is corrupt authority, not a
        // reason to fall back to something weaker.
        (Some(_), _) => return None,
        // Legacy outgoing build with no published record: the parent link names this pid
        // before and after the birth read, and it flips at the parent's exit, before the pid
        // can recycle, so the record is the live parent's own. Capturing it lets the watch stop
        // consulting the process tree.
        (None, Some(live)) if creator == pid => {
            if creator_after() != pid {
                return None;
            }
            ParentWitness::Birth(live)
        }
        (None, None) if creator == pid => {
            // On macOS libproc answers for a live same-uid process, so no record
            // is an anomaly, said below as the one signal that this machine's
            // handoffs run on the weaker witness — or the parent is already
            // EXITING: its birth read
            // fails about a millisecond before this link moves (the order measured
            // at `read_process_birth`), so the line names both causes. Off macOS
            // the link IS the documented witness.
            #[cfg(target_os = "macos")]
            crate::logging::stderr_line!(
                "aterm-gui: seamless handoff: no kernel birth record for parent {pid} \
                 (libproc declined it, or the parent is exiting); falling back to the \
                 process link for parent-death detection"
            );
            ParentWitness::ForkLink
        }
        // No live creator and nothing published: there is no witness at all.
        (None, _) => return None,
    };
    Some(AttestedParent { pid, witness })
}

#[cfg(unix)]
#[must_use]
fn attest_handoff_parent(
    pid: libc::pid_t,
    published: Option<ProcessBirth>,
) -> Option<AttestedParent> {
    // SAFETY: `getppid` is a side-effect-free libc getter.
    let parent_link = || unsafe { libc::getppid() };
    attest_handoff_parent_from(pid, published, parent_link(), parent_link)
}

/// A handoff parent whose identity was proven at admission. Its fields are private and
/// [`attest_handoff_parent_from`] is the only producer, so holding one is the proof and the only
/// question left is whether that process is alive. The value, not a bare pid, travels from
/// [`prearm_incoming_fds_from`] through [`take_commit_fd_from`] into [`CommitReceiver`], so no
/// later stage can re-derive a weaker claim.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AttestedParent {
    pid: libc::pid_t,
    witness: ParentWitness,
}

#[cfg(unix)]
impl AttestedParent {
    /// The attested process's pid — a name for files it left (its crash
    /// journal, which the successor takes over at Commit), never a proof that
    /// something at that pid now is it ([`Self::still_alive`] is).
    #[must_use]
    pub(crate) fn pid(self) -> u32 {
        u32::try_from(self.pid).unwrap_or(0)
    }

    /// When the attested process started (unix nanos), from the birth record admission
    /// verified; `None` for a fork-link attestation. Bounds which of that pid's crash journals
    /// is this process's run, for the successor's take-over at Commit.
    #[must_use]
    pub(crate) fn birth_unix_nanos(self) -> Option<u128> {
        match self.witness {
            ParentWitness::Birth(birth) => Some(
                u128::from(birth.seconds) * 1_000_000_000 + u128::from(birth.microseconds) * 1_000,
            ),
            ParentWitness::ForkLink => None,
        }
    }

    /// Is the attested process — that exact process, not merely something at
    /// its pid — still running?
    #[must_use]
    pub(crate) fn still_alive(self) -> bool {
        match self.witness {
            ParentWitness::Birth(birth) => read_process_birth(self.pid) == Some(birth),
            ParentWitness::ForkLink => {
                // SAFETY: `getppid` is a side-effect-free libc getter.
                let creator = unsafe { libc::getppid() };
                creator == self.pid
            }
        }
    }

    /// Wait until the attested process has exited (`true`), or until
    /// `deadline` with it still running (`false`). The kernel's exit event
    /// wakes the wait where one is wired (Darwin's kqueue, Linux's pidfd), but
    /// [`Self::still_alive`] alone judges: after an event it does not agree
    /// with yet, or with no event to wait on, it is asked again on
    /// [`aterm_uds::deadline::ExitWait`]'s fallback tick.
    #[must_use]
    pub(crate) fn exited_by(self, deadline: std::time::Instant) -> bool {
        // The watch is on whatever holds the pid when it is taken. The witness
        // still alive after that proves it was this process: one that ended
        // never comes back at its pid with its birth, nor as our creator.
        let mut exit = aterm_uds::deadline::ExitWait::on_pid(self.pid());
        while self.still_alive() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            exit.wait(left);
        }
        true
    }

    /// Is `peer`, the kernel-attested pid of a live listener this process just dialed
    /// (`LOCAL_PEERPID`, which XNU records from the listening socket at `connect(2)`), this
    /// attested process, still alive? The rendezvous's identity bind: the claim secret goes only
    /// to the parent itself, not to any same-uid process holding the published path. Unlike
    /// [`Self::owns_retiring_socket`], an unreadable birth record refuses: a listener about to
    /// receive the claim belongs to a parent that must be alive now.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub(crate) fn owns_live_listener(self, peer: u32) -> bool {
        u32::try_from(self.pid).ok() == Some(peer) && self.still_alive()
    }

    /// A lingering listener may still report its deceased creator. Refuse a
    /// different PID, or a reused PID whose birth no longer matches our witness.
    pub(crate) fn owns_retiring_socket(self, peer: u32) -> bool {
        if u32::try_from(self.pid).ok() != Some(peer) {
            return false;
        }
        match (self.witness, read_process_birth(self.pid)) {
            (ParentWitness::Birth(expected), Some(current)) => current == expected,
            (_, None) => true,
            (ParentWitness::ForkLink, Some(_)) => false,
        }
    }

    /// The authority pairs to restore onto the boot-apply re-exec image. A `Birth` witness
    /// republishes the record so the re-exec'd image can re-attest without a parent link even
    /// when the original outgoing process published none: we hold only a `Birth` witness we
    /// proved, so this passes a proof on.
    fn exec_env(self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        let mut pairs = vec![ENV_PARENT_PID.pair(self.pid.to_string())];
        if let ParentWitness::Birth(birth) = self.witness {
            pairs.push(ENV_PARENT_BIRTH.pair(birth.to_wire()));
        }
        pairs
    }
}

/// Attestations other modules' tests need and cannot mint: the fields are
/// private and [`attest_handoff_parent_from`] is the only producer.
/// macOS-only, like the one module whose tests use them (`handoff_rendezvous`).
#[cfg(all(test, target_os = "macos"))]
impl AttestedParent {
    /// Attest the live process at `pid` by its current birth record, through
    /// the real admission function's no-live-creator arm — exactly how a
    /// LaunchServices successor (parent link 1) attests its outgoing process.
    pub(crate) fn attest_live_for_test(pid: libc::pid_t) -> Option<Self> {
        attest_handoff_parent_from(pid, read_process_birth(pid), 1, || 1)
    }

    /// An attestation of `pid` whose birth witness names an instant no live
    /// process there was born at: the attested parent is gone and its pid now
    /// names someone else (or no one).
    pub(crate) fn recycled_for_test(pid: libc::pid_t) -> Self {
        Self {
            pid,
            witness: ParentWitness::Birth(ProcessBirth {
                seconds: 1,
                microseconds: 0,
            }),
        }
    }
}

/// The parental-authority environment an outgoing process publishes to its successor, encoded
/// here beside the decoding in [`prearm_incoming_fds_from`] so the two cannot drift.
///
/// The birth record is omitted when the kernel will not give us our own: the successor refuses
/// a published record it cannot corroborate, so publishing one would turn an unavailable probe
/// into a failed update, where omitting it costs only a parent-link attestation.
///
/// It also says what this parent can read of the successor's exit ([`ENV_EXIT_CAPS`]), from the
/// same list, so neither lane carries one without the other. An older successor never sees it:
/// the automatic lane only hands off to a newer build, and this build captures and consumes the
/// name.
#[cfg(unix)]
#[must_use]
pub(crate) fn outgoing_parent_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut pairs = vec![ENV_PARENT_PID.pair(std::process::id().to_string())];
    if let Some(birth) = own_process_birth() {
        pairs.push(ENV_PARENT_BIRTH.pair(birth.to_wire()));
    }
    pairs.push(ENV_EXIT_CAPS.pair(EXIT_CAPS_PASSING));
    pairs
}

/// Whether the outgoing process declared that it reads
/// [`EXIT_SUCCESSOR_PASSING`] as a passing failure ([`ENV_EXIT_CAPS`] names
/// [`EXIT_CAPS_PASSING`]) — taken out of the snapshot, so no helper or shell
/// ever sees the name. Absent, unreadable or naming anything else: `false`,
/// the answer every shipped parent (0.100.0 and older) is owed.
#[cfg(unix)]
#[must_use]
pub(crate) fn take_parent_reads_passing_exit_from(env: &mut HandoffEnv) -> bool {
    env.take(ENV_EXIT_CAPS)
        .and_then(|caps| caps.into_string().ok())
        .is_some_and(|caps| caps.split(',').any(|cap| cap.trim() == EXIT_CAPS_PASSING))
}

/// The turn-id count this successor continues from, given the manifest's `next_turn_id`: past
/// it by one more overlap reserve (`control::TURN_ID_RESERVE`). A producer before the reserve
/// (0.98) carried its bare count and kept minting in the overlap; ids only have to rise, so a
/// reserving producer loses nothing.
#[must_use]
pub(crate) fn intake_turn_floor(next_turn_id: u64) -> u64 {
    next_turn_id.saturating_add(crate::control::TURN_ID_RESERVE)
}

/// The exit status of a successor that refused the carried control-socket witness
/// (`main_entry`'s fixed-socket check). The witness is sampled when the attempt begins and the
/// parent's self-healing listener rebind can move the socket's inode under a held successor:
/// the parent's endpoint state, not a verdict on this build. So a parent that reads
/// [`EXIT_SUCCESSOR_PASSING`] is told it was transient (its next attempt resamples), and an
/// older one gets the `74` it has always filed.
#[cfg(unix)]
#[must_use]
pub(crate) fn endpoint_refusal_exit(parent_reads_passing: bool) -> i32 {
    if parent_reads_passing {
        EXIT_SUCCESSOR_PASSING
    } else {
        74
    }
}

/// Re-arm every descriptor named by an incoming handoff immediately on process
/// entry, before the updater can spawn codesign/PlistBuddy/spctl helpers. The
/// returned exact fd list is passed only to the updater's final same-process
/// exec seam, which temporarily clears these flags and re-arms them if exec
/// returns. Malformed/dead entries are omitted, making the later handoff fail
/// closed rather than leaking an fd to an intermediate subprocess.
#[cfg(unix)]
pub(crate) struct PrearmedIncomingFds {
    fds: Vec<i32>,
    recognizable: bool,
    valid: bool,
    parent: Option<AttestedParent>,
}

#[cfg(unix)]
impl PrearmedIncomingFds {
    pub(crate) fn final_exec_fds(&self) -> &[i32] {
        if self.valid { &self.fds } else { &[] }
    }

    /// Environment restored only onto the updater's final same-process exec image, never the
    /// ambient env ([`prearm_incoming_fds_from`] removed it so no codesign/PlistBuddy/spctl
    /// helper sees it). The re-exec'd binary re-runs prearm, which requires an inherited
    /// (fork-lane) handoff to carry the parental identity; without this pair it would call the
    /// handoff malformed and exit before its readiness proof, and the parent would read
    /// `ChildDied`.
    pub(crate) fn final_exec_env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        if !self.valid {
            return Vec::new();
        }
        self.parent
            .map(AttestedParent::exec_env)
            .unwrap_or_default()
    }

    /// Everything the boot apply's re-exec must carry onto the new image: the handoff names still
    /// in the snapshot `env`, then the attested parent's pair ([`Self::final_exec_env`]).
    /// Authority-gated like the descriptors: a refused handoff carries nothing.
    pub(crate) fn final_exec_env_with(
        &self,
        env: &HandoffEnv,
    ) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        if !self.valid {
            return Vec::new();
        }
        let mut pairs = env.exec_env();
        pairs.extend(self.final_exec_env());
        pairs
    }

    pub(crate) fn blocks_boot_apply(&self) -> bool {
        self.recognizable && !self.valid
    }

    /// A recognizable but malformed handoff is not an ordinary cold start. Its
    /// descriptor names have already been closed and its authority variables
    /// cleared; startup must stop before an updater helper or user shell can be
    /// spawned. This prevents a stale second adoption from closing a descriptor
    /// number that the process has since reused for an unrelated resource.
    pub(crate) fn rejects_boot(&self) -> bool {
        self.blocks_boot_apply()
    }

    /// The attested handoff parent: named for the pid, but it carries the proof, so no later
    /// stage re-derives one from the scrubbed environment.
    pub(crate) fn parent_pid(&self) -> Option<AttestedParent> {
        self.valid.then_some(self.parent).flatten()
    }
}

/// The names the prearm reads, and the ones a refused handoff's authority is cleared of. The
/// target identity, proof term and control-socket witness are not among them (a refused boot
/// never reads them).
#[cfg(unix)]
const PREARM_KEYS: &[HandoffName] = &[
    ENV_MANIFEST,
    ENV_FDS,
    ENV_NONCE,
    ENV_LAYOUT,
    ENV_READY_FD,
    ENV_COMMIT_FD,
    ENV_RENDEZVOUS,
    ENV_CLAIM,
    ENV_PARENT_PID,
    ENV_PARENT_BIRTH,
    // The parent's grant capabilities ride only beside a rendezvous, which is
    // macOS's alone.
    #[cfg(target_os = "macos")]
    crate::handoff_rendezvous::ENV_GRANT_CAPS,
];

#[cfg(unix)]
fn clear_handoff_env(env: &mut HandoffEnv) {
    for &key in PREARM_KEYS {
        env.remove(key);
    }
}

#[cfg(unix)]
fn parse_named_fd_bytes(entry: &[u8]) -> Option<i32> {
    let equals = entry.iter().position(|byte| *byte == b'=')?;
    let tail = &entry[equals + 1..];
    let colon = tail.iter().position(|byte| *byte == b':')?;
    std::str::from_utf8(&tail[..colon])
        .ok()?
        .trim()
        .parse::<i32>()
        .ok()
}

/// [`prearm_incoming_fds_from`] over the process environment: the env-shaped
/// entry point the handoff suites drive (`handoff_env::through_process_env`).
#[cfg(all(unix, test))]
pub(crate) fn prearm_incoming_fds() -> PrearmedIncomingFds {
    crate::handoff_env::through_process_env(PREARM_KEYS, prearm_incoming_fds_from)
}

/// The successor's entry-point posture repair, over the handoff snapshot `main_entry` captured
/// ([`HandoffEnv::capture`]) before any helper, session, window or user code: re-arm CLOEXEC on
/// every descriptor an incoming handoff names, attest the outgoing parent, and remove the
/// authority names from `env`.
///
/// B3 — PROCESS-GROUP CONTAINMENT, RESIDUAL GAP. On the fork lane the parent puts the
/// candidate in its own process group with `setpgid(0, 0)` in
/// `run_handoff_worker`'s `pre_exec`, which is what makes that file's
/// `kill(-pid, SIGKILL)` sweep the candidate's `ditto`/`codesign`/`spctl`
/// helpers instead of only its leader. A LaunchServices launch has no pre-exec
/// hook, so the successor contains ITSELF: B3 landed as
/// `app_update_handoff::contain_own_process_group`, called from
/// `crate::main_entry` ahead of the boot apply and of anything else able to run
/// another program, and failing closed on a foreign group. It is not called in
/// this function because unit tests exercise this function in-process, where a
/// process-wide `setpgid` would move the test binary out of its harness's group
/// (that function's doc carries the rest of the argument).
///
/// * The ordering: until the successor contains itself it sits in whatever group the launcher
///   left, so a rejection then signals a group the candidate is not in, and a recycled
///   group-leader pid can make `kill(-pid)` reach strangers (a group has no birth record to
///   attest, unlike [`AttestedParent`]).
/// * The parent learns the group only by asking: the readiness wire has no room for a pgid, so
///   the parent checks `getpgid(pid) == pid` at the moment of use
///   (`app_update_handoff::candidate_leads_its_own_group`, used by `kill_corroborated_candidate`)
///   and otherwise sends `kill(pid)` alone, logged. The fork lane's ordering of signalling the
///   group before any wait (`emergency_kill_and_reap_handoff_child`) has no equivalent here.
#[cfg(unix)]
pub(crate) fn prearm_incoming_fds_from(env: &mut HandoffEnv) -> PrearmedIncomingFds {
    let manifest_present = env.present(ENV_MANIFEST);
    let fds_present = env.present(ENV_FDS);
    let nonce_present = env.present(ENV_NONCE);
    let layout_present = env.present(ENV_LAYOUT);
    let ready_present = env.present(ENV_READY_FD);
    let commit_present = env.present(ENV_COMMIT_FD);
    // The out-of-band lane's two names, read here beside every other handoff name so one
    // function decides what a recognizable handoff environment is.
    let rendezvous_present = env.present(ENV_RENDEZVOUS);
    let claim_present = env.present(ENV_CLAIM);
    let parent_present = env.present(ENV_PARENT_PID);
    let birth_present = env.present(ENV_PARENT_BIRTH);
    let published_birth = env
        .string(ENV_PARENT_BIRTH)
        .and_then(|value| ProcessBirth::from_wire(&value));
    let parent = env
        .string(ENV_PARENT_PID)
        .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
        .and_then(|pid| attest_handoff_parent(pid, published_birth));
    // Parent identity is typed state from here on; the raw value must not reach updater helpers
    // or user shells (the re-exec republishes it from the attestation, `final_exec_env`).
    env.remove(ENV_PARENT_PID);
    env.remove(ENV_PARENT_BIRTH);
    let recognizable = manifest_present
        || fds_present
        || nonce_present
        || layout_present
        || ready_present
        || commit_present
        || rendezvous_present
        || claim_present
        || parent_present
        || birth_present;
    let mut authority_valid = true;
    let mut fds = Vec::new();
    // Keep every syntactically enumerable descriptor separate from the set that
    // passed full authority parsing. On any recognizable rejection they are all
    // closed exactly once before fd numbers can be reused.
    let mut named_fds = Vec::new();
    if fds.try_reserve_exact(MAX_HANDOFF_SESSIONS + 2).is_err() {
        authority_valid = false;
    }
    if let Some(wire) = env.string(ENV_FDS) {
        if wire.len() > MAX_HANDOFF_FDS_WIRE_BYTES {
            authority_valid = false;
        }
        let mut entries = 0usize;
        for entry in wire.split(',').filter(|entry| !entry.is_empty()) {
            entries = entries.saturating_add(1);
            let named_fd = parse_named_fd(entry);
            let parsed = parse_fd_entry(entry);
            if parsed.is_none() {
                authority_valid = false;
            }
            if entries > MAX_HANDOFF_SESSIONS {
                authority_valid = false;
            }
            // Rearm streaming even after the authority cap is exceeded: a
            // syntactically named live fd must not leak to updater helpers.
            if let Some(fd) = named_fd
                && fd >= 3
                && unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0
            {
                named_fds.push(fd);
                if aterm_pty::set_cloexec(fd, true).is_err() {
                    authority_valid = false;
                } else if parsed.is_some() {
                    fds.push(fd);
                }
            } else {
                authority_valid = false;
            }
        }
    } else if fds_present {
        // A non-UTF-8 wire is recognizable authority, never an empty handoff.
        authority_valid = false;
        use std::os::unix::ffi::OsStrExt as _;
        if let Some(wire) = env.os(ENV_FDS) {
            for entry in wire.as_bytes().split(|byte| *byte == b',') {
                if let Some(fd) = parse_named_fd_bytes(entry)
                    && fd >= 3
                    && unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0
                {
                    named_fds.push(fd);
                    let _ = aterm_pty::set_cloexec(fd, true);
                }
            }
        }
    }
    for key in [ENV_READY_FD, ENV_COMMIT_FD] {
        let present = env.present(key);
        if let Some(value) = env.string(key) {
            if let Ok(fd) = value.trim().parse::<i32>() {
                if fd >= 3 && unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 {
                    named_fds.push(fd);
                    if aterm_pty::set_cloexec(fd, true).is_err() {
                        authority_valid = false;
                    } else {
                        fds.push(fd);
                    }
                } else {
                    authority_valid = false;
                }
            } else {
                authority_valid = false;
            }
        } else if present {
            authority_valid = false;
        }
    }
    // A partial or legacy-shaped env set is unrecognizable authority: its descriptors are
    // closed here rather than carried to an exec image that would refuse them.
    let modern = handoff_is_modern_overlap(HandoffEnvPresence {
        manifest: manifest_present,
        fds: fds_present,
        nonce: nonce_present,
        layout: layout_present,
        ready: ready_present,
        commit: commit_present,
        rendezvous: rendezvous_present,
        claim: claim_present,
    });
    if recognizable && !modern {
        authority_valid = false;
    }
    if modern != parent.is_some() || (parent_present && parent.is_none()) {
        authority_valid = false;
    }
    // The birth record is optional authority (a legacy outgoing build publishes none), but an
    // unparseable one is corruption, not absence, and must not degrade admission to the weaker
    // witness.
    if birth_present && published_birth.is_none() {
        authority_valid = false;
    }
    fds.sort_unstable();
    if fds.windows(2).any(|pair| pair[0] == pair[1]) {
        authority_valid = false;
    }
    fds.dedup();
    if !authority_valid {
        named_fds.sort_unstable();
        named_fds.dedup();
        for fd in named_fds {
            aterm_pty::close_fd(fd);
        }
        if recognizable {
            clear_handoff_env(env);
        }
        fds.clear();
    }
    PrearmedIncomingFds {
        fds,
        recognizable,
        valid: authority_valid,
        parent,
    }
}

#[cfg(not(unix))]
pub(crate) struct PrearmedIncomingFds;

#[cfg(not(unix))]
impl PrearmedIncomingFds {
    pub(crate) fn final_exec_fds(&self) -> &[i32] {
        &[]
    }

    /// The re-exec's pairs (the unix twin's doc): no handoff exists off unix, but
    /// a snapshot's stray names ride the exec as they rode `environ`.
    pub(crate) fn final_exec_env_with(
        &self,
        env: &HandoffEnv,
    ) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        env.exec_env()
    }

    pub(crate) fn blocks_boot_apply(&self) -> bool {
        false
    }

    pub(crate) fn rejects_boot(&self) -> bool {
        false
    }

    pub(crate) fn parent_pid(&self) -> Option<i32> {
        None
    }
}

#[cfg(not(unix))]
pub(crate) fn prearm_incoming_fds_from(_env: &mut HandoffEnv) -> PrearmedIncomingFds {
    PrearmedIncomingFds
}

/// The readiness channel owned by the incoming process. Its nonce is the
/// authenticated manifest nonce; signaling computes the proof from sessions
/// that actually reached the child's live pool, never from the outgoing claim.
#[cfg(unix)]
pub(crate) struct ReadySignal {
    fd: std::os::fd::OwnedFd,
    nonce: String,
    layout_digest: [u8; 32],
    screen_digest: [u8; 32],
    /// The build/commit the parent expects this process to be, already checked equal to our own
    /// `build_info` by [`take_target_identity_from`]. Hashing the agreed value keeps "I am the
    /// binary you asked for" a real property, while a disagreement is a logged refusal rather
    /// than a poisoned digest.
    target: HandoffTarget,
}

/// The exact binary identity the outgoing process authorized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HandoffTarget {
    pub build: u64,
    /// Already reduced by [`normalize_commit`].
    pub commit: String,
}

impl HandoffTarget {
    /// This process's own compiled-in identity. Used when the parent published
    /// no expectation (an older parent).
    #[must_use]
    pub(crate) fn own() -> Option<Self> {
        Some(Self {
            build: crate::build_info::build_number()
                .parse::<u64>()
                .unwrap_or(0),
            commit: normalize_commit(crate::build_info::git_commit())?,
        })
    }
}

/// Encode the parent's expectation for [`ENV_TARGET`]. A spoofed value can only lose a handoff:
/// the parent compares the child's proof against its own expectation, so a forged target yields
/// a digest the parent rejects.
#[must_use]
#[cfg(unix)]
pub(crate) fn encode_target_identity(build: u64, commit: &str) -> String {
    format!("{build} {}", commit.trim())
}

/// Read a parent's [`ENV_TARGET`] value: `"<build> <commit>"`, the commit reduced
/// by [`normalize_commit`]. `None` for anything else. The one reader of the
/// spelling [`encode_target_identity`] writes, split out of
/// [`take_target_identity_from`] so the cross-version guard can hand it the strings
/// shipped parents really published (`seamless_fixture_tests`).
#[must_use]
pub(crate) fn parse_target_identity(raw: &str) -> Option<HandoffTarget> {
    let (build, commit) = raw.trim().split_once(' ')?;
    build
        .parse::<u64>()
        .ok()
        .zip(normalize_commit(commit))
        .map(|(build, commit)| HandoffTarget { build, commit })
}

/// Does the parent's [`ENV_TARGET`] name THIS build? A non-consuming peek for the
/// boot-apply gate (`take_target_identity` consumes it moments later, in the same
/// single-threaded launcher). When it does, this process IS the authorized
/// candidate — an ACTIVATION successor, or a swapped download successor already
/// re-exec'd into the target — and a further boot swap into any newer stage on
/// disk would turn it into a build the parent did not authorize: the re-exec'd
/// image refuses `take_target_identity`, closes the adopted fds and exits, and
/// the parent books a `ChildDied` structural failure for a candidate that was
/// perfectly healthy (found by the 2026-08-19 audit; the newer stage on disk is
/// left for the successor's own apply lane instead). Malformed
/// values answer `false`: the
/// consuming reader is the one that diagnoses them.
#[must_use]
pub(crate) fn target_identity_names_this_build(env: &HandoffEnv) -> bool {
    let Some(raw) = env.os(ENV_TARGET) else {
        return false;
    };
    let raw = raw.to_string_lossy();
    let Some((build, _commit)) = raw.trim().split_once(' ') else {
        return false;
    };
    let own = crate::build_info::build_number()
        .parse::<u64>()
        .unwrap_or(0);
    build.parse::<u64>().ok() == Some(own)
}

/// [`take_target_identity_from`] over the process environment: the env-shaped
/// entry point the handoff suites drive (`handoff_env::through_process_env`).
#[cfg(all(test, unix))]
#[must_use]
pub(crate) fn take_target_identity() -> Option<HandoffTarget> {
    crate::handoff_env::through_process_env(&[ENV_TARGET], take_target_identity_from)
}

/// Consume [`ENV_TARGET`] from the handoff snapshot and prove this process is the binary the
/// parent authorized. Returns the agreed identity, or `None` after logging which component
/// disagreed, so the refusal is never a bare `AdoptionMismatch`.
#[must_use]
pub(crate) fn take_target_identity_from(env: &mut HandoffEnv) -> Option<HandoffTarget> {
    // One-shot by construction: taken out of the snapshot.
    let raw = env.take(ENV_TARGET).and_then(|v| v.into_string().ok());
    let own = HandoffTarget::own();
    let Some(raw) = raw else {
        // Older parent: it hashes its ticket's target and we hash our own build_info.
        return own;
    };
    let Some(own) = own else {
        // Both sinks: a LaunchServices successor has no terminal on either stream, and the
        // parent sees only the readiness pipe close (`ChildDied`). The log file is the durable
        // sink; stderr serves a developer running the binary from a shell.
        aterm_log::error!(
            "seamless handoff refused: this build reports commit `{}`, which is not a usable \
             identity",
            crate::build_info::git_commit()
        );
        crate::logging::stderr_line!(
            "aterm-gui: seamless handoff refused: this build reports commit \
             `{}`, which is not a usable identity",
            crate::build_info::git_commit()
        );
        return None;
    };
    let Some(expected) = parse_target_identity(&raw) else {
        aterm_log::error!("seamless handoff refused: malformed target identity `{raw}`");
        crate::logging::stderr_line!(
            "aterm-gui: seamless handoff refused: malformed target identity `{raw}`"
        );
        return None;
    };
    if expected != own {
        // The successor did not become the authorized build: a boot apply that answered
        // `Deferred` (most often, the installed bundle cannot be the swap's rollback source)
        // leaves this image the old build, which must refuse rather than adopt under a proof it
        // cannot honour. Naming the boot apply turns the parent's bare `ChildDied` into a
        // diagnosis.
        aterm_log::error!(
            "seamless handoff refused: the outgoing process authorized build {} commit {}, but \
             this binary is build {} commit {} — this successor did not become the authorized \
             build (see the `update apply (boot)` line above for why the swap did not happen). \
             Closing the readiness channel; the outgoing process keeps every session.",
            expected.build,
            expected.commit,
            own.build,
            own.commit
        );
        crate::logging::stderr_line!(
            "aterm-gui: seamless handoff refused: the outgoing process authorized \
             build {} commit {}, but this binary is build {} commit {}",
            expected.build,
            expected.commit,
            own.build,
            own.commit
        );
        return None;
    }
    Some(expected)
}

/// The exit status a refusing successor leaves with when what stopped it was passing:
/// `EX_TEMPFAIL` from `sysexits.h`, "temporary failure; the user is invited to retry". It goes
/// only to a parent that declared it reads it ([`ENV_EXIT_CAPS`]); parents through 0.100.0 file
/// every `Exited` structural and keep the counted trial launch. Two paths produce it: a
/// successor whose boot swap left it the outgoing build's image for a reason that is no verdict
/// on the target ([`passing_refusal_exit`]), and one whose intake refused the outgoing build's
/// artifacts whole ([`refused_intake_leave`]).
#[cfg(unix)]
pub(crate) const EXIT_SUCCESSOR_PASSING: i32 = 75;

/// Whether a successor that refused its target leaves with [`EXIT_SUCCESSOR_PASSING`] instead
/// of the ordinary refusal's clean `0`.
///
/// THE FIELD SHAPE (round four, plan item 2): on the download lane the outgoing
/// process launched ITS OWN image, which boot-applied the staged build and
/// re-exec'd into it (a fork-lane child, or a successor an older build launches,
/// still does; since 2026-10-19 the launched lane's parent swaps first,
/// `aterm_update::swap_staged_for_launch`, and files its own refusal). When that apply is `Deferred` for a moment — the apply lock
/// held past its wait by a sibling launch, a `codesign` that ran out of the
/// apply budget on a loaded desk ([`aterm_update::is_passing_refusal`]) — the
/// image stays the old build, [`take_target_identity_from`] rightly refuses the
/// target, and the process returned from `main_entry` with `0`. The parent read
/// `Exited`, filed it STRUCTURAL, and two such moments converged a healthy build
/// for a day. The same refusal now tells the parent it was a moment.
///
/// All four facts are required: the target was refused (`!target_admitted`), it did not name
/// this build (an activation successor that is the target never gets here), the boot apply
/// passed no verdict on the stage, and the parent declared it reads the status as meant
/// (`parent_reads_passing`, [`ENV_EXIT_CAPS`]). Every other refusal keeps its `0` and the leave
/// [`refused_intake_leave`] gives it.
#[cfg(unix)]
#[must_use]
pub(crate) fn passing_refusal_exit(
    boot_apply: &aterm_update::ApplyOutcome,
    target_names_this_build: bool,
    target_admitted: bool,
    parent_reads_passing: bool,
) -> Option<i32> {
    let no_verdict_on_the_stage = match boot_apply {
        // A stage left for a window's lane is no verdict on it either.
        aterm_update::ApplyOutcome::NoUpdate
        | aterm_update::ApplyOutcome::NotApplicable
        | aterm_update::ApplyOutcome::LeftForSession { .. } => true,
        aterm_update::ApplyOutcome::Deferred(reason) => aterm_update::is_passing_refusal(reason),
        // A crash-loop revert is the boot's verdict on the build (and its caller has
        // exited before ever reaching this).
        aterm_update::ApplyOutcome::ReExecFailed(_) | aterm_update::ApplyOutcome::Reverted(_) => {
            false
        }
    };
    (no_verdict_on_the_stage
        && !target_admitted
        && !target_names_this_build
        && parent_reads_passing)
        .then_some(EXIT_SUCCESSOR_PASSING)
}

/// Whether this launch was offered a handoff's sessions: the descriptor list ([`ENV_FDS`]) the
/// fork lane's parent sets and the launched lane's granted rendezvous claim installs into the
/// snapshot. Read before [`take_incoming_from`], which consumes it.
#[must_use]
pub(crate) fn incoming_offered_in(env: &HandoffEnv) -> bool {
    env.present(ENV_FDS)
}

/// [`incoming_offered_in`] over the process environment (the suites' shape).
#[cfg(all(test, unix))]
#[must_use]
pub(crate) fn incoming_offered() -> bool {
    crate::handoff_env::through_process_env(&[ENV_FDS], |env| incoming_offered_in(env))
}

/// Whether a successor whose overlap authority is incomplete (`overlap_degraded`: a readiness or
/// Commit channel arrived, and the pair was not admitted) must exit before any window instead of
/// booting fresh: when it adopted sessions (they are the parked parent's too), and when it was
/// offered some and the intake refused them all, since a cold boot would take `session.toml`,
/// crash journals and a shell while the parent still owns every session. Stray channel
/// variables with no offered sessions still boot fresh.
#[must_use]
pub(crate) fn overlap_intake_exits(overlap_degraded: bool, adopted: usize, offered: bool) -> bool {
    overlap_degraded && (adopted > 0 || offered)
}

/// How a successor leaves an overlap it will not take ([`overlap_intake_exits`]) once
/// [`passing_refusal_exit`] sent no 75 for the boot swap; decided by [`refused_intake_leave`].
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusedIntakeLeave {
    /// Return from `main_entry`: the ordinary refusal's `0`, which every parent files
    /// structural, keeping the counted trial launch. Sessions were adopted, or (to a parent that
    /// reads 75) the refusal was this build's own verdict ([`WholeRefusal::Own`]) or no whole
    /// refusal at all.
    Ordinary,
    /// Give the counted trial launch back and exit [`EXIT_SUCCESSOR_PASSING`]:
    /// the refusal was the outgoing build's content ([`WholeRefusal::Producer`]),
    /// and the parent declared that it reads 75 as a passing failure
    /// ([`ENV_EXIT_CAPS`]). Filing that refusal structural would latch a healthy
    /// new build for its predecessor's content — L1.
    Passing,
    /// Give the counted trial launch back and leave the ending to the parent
    /// ([`await_parents_end`]): nothing was adopted, and the parent never declared that it reads
    /// 75 (every shipped parent).
    ///
    /// Such a parent files every `Exited` structural, 75 included, and keeps the counted launch,
    /// so two refusals would latch the build and the next cold launch revert it. What it files
    /// as no evidence against the bytes is a candidate it ended: the refused intake already
    /// closed the readiness channel, so the parent reads proof EOF, finds this process running,
    /// kills it and files `Unobserved`, retried on its bounded unexplained budget with the
    /// launch forgiven. Waiting gets that verdict without booting fresh while the parent still
    /// owns every session.
    ParentEndsIt,
}

#[cfg(unix)]
impl RefusedIntakeLeave {
    /// Whether this leave gives back the boot-trial launch this process counted
    /// ([`give_back_refused_intake_launch`]): both leaves that say the refusal is
    /// not a verdict on the bytes. The ordinary one leaves it to the parent's
    /// rule, which keeps it for a structural death.
    #[must_use]
    pub(crate) fn gives_back_trial_launch(self) -> bool {
        matches!(self, Self::Passing | Self::ParentEndsIt)
    }
}

/// How a successor whose overlap authority is incomplete leaves
/// ([`RefusedIntakeLeave`]), from what it adopted, whose content a whole
/// refusal was about ([`IncomingHandoff::refusal`]), and whether the parent
/// declared that it reads [`EXIT_SUCCESSOR_PASSING`]
/// ([`take_parent_reads_passing_exit_from`]).
///
/// Adopted sessions leave as they always have. With none, a parent that did not declare the
/// capability gets the ending it files as no evidence (it kills the candidate), whatever the
/// refusal; one that did gets 75 for the producer's content and the ordinary `0` for this
/// build's own verdict.
#[cfg(unix)]
#[must_use]
pub(crate) fn refused_intake_leave(
    adopted: usize,
    refusal: Option<WholeRefusal>,
    parent_reads_passing: bool,
) -> RefusedIntakeLeave {
    if adopted > 0 {
        RefusedIntakeLeave::Ordinary
    } else if !parent_reads_passing {
        RefusedIntakeLeave::ParentEndsIt
    } else if refusal == Some(WholeRefusal::Producer) {
        RefusedIntakeLeave::Passing
    } else {
        RefusedIntakeLeave::Ordinary
    }
}

/// Give back the boot-trial launch this process counted for `build` when `leave` says the
/// refusal was no verdict on the bytes, by name ([`aterm_update::forgive_own_trial_launch`]),
/// as the rendezvous and fixed-socket refusals in `main_entry` do. The parent settling the same
/// death gives back nothing more: this build's parent asks by the same name, and a shipped
/// parent's count-based ask finds the count already where its snapshot left it. `true` when a
/// launch was given back.
#[cfg(unix)]
pub(crate) fn give_back_refused_intake_launch(leave: RefusedIntakeLeave, build: u64) -> bool {
    leave.gives_back_trial_launch() && aterm_update::forgive_own_trial_launch(build)
}

/// How long [`RefusedIntakeLeave::ParentEndsIt`] waits for the parent to end
/// this candidate. The parent kills it within one poll slice of the proof EOF;
/// a parent that somehow missed the EOF still kills it at its readiness
/// deadline (30 s, at most 120 s through its development seam), which this
/// outlasts. A hang detector, never a schedule: past it the candidate leaves on
/// its own and says so.
#[cfg(unix)]
pub(crate) const PARENTS_END_BOUND: std::time::Duration = std::time::Duration::from_secs(180);

/// How a [`await_parents_end`] wait ended, when it returned at all — the
/// commonest end is the parent's `SIGKILL`, which returns nothing.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParentsEnd {
    /// The attested parent is gone, or none was attested: nobody is left to
    /// read this candidate's status.
    ParentGone,
    /// The bound passed with the parent still running.
    BoundPassed,
}

/// Wait, bounded by `bound`, for the attested `parent` to end this candidate
/// ([`AttestedParent::exited_by`]). Holds nothing while it waits: the caller
/// has already closed every channel and adopted master.
#[cfg(unix)]
#[must_use]
pub(crate) fn await_parents_end(
    parent: Option<AttestedParent>,
    bound: std::time::Duration,
) -> ParentsEnd {
    match parent {
        Some(parent) if !parent.exited_by(std::time::Instant::now() + bound) => {
            ParentsEnd::BoundPassed
        }
        _ => ParentsEnd::ParentGone,
    }
}

#[cfg(unix)]
impl ReadySignal {
    #[must_use]
    pub(crate) fn raw_fd(&self) -> i32 {
        use std::os::fd::AsRawFd as _;
        self.fd.as_raw_fd()
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        fd: std::os::fd::OwnedFd,
        nonce: &str,
        layout_digest: [u8; 32],
        screen_digest: [u8; 32],
    ) -> Self {
        Self {
            fd,
            nonce: nonce.to_owned(),
            layout_digest,
            screen_digest,
            target: HandoffTarget::own().unwrap_or(HandoffTarget {
                build: 0,
                commit: "unknown".to_string(),
            }),
        }
    }

    /// Compute the fixed adoption proof before either channel is touched. This
    /// lets the child provision its Commit waiter before ProofReady becomes
    /// observable by the parent.
    #[must_use]
    pub(crate) fn proof(&self, adopted: &[SessionIdentity]) -> Option<AdoptionProof> {
        adoption_proof(
            &self.nonce,
            self.target.build,
            &self.target.commit,
            &self.layout_digest,
            &self.screen_digest,
            adopted,
        )
        .ok()
    }

    /// Publish a proof that was computed before the Commit waiter was spawned.
    /// The child remains readerless afterward; only the separate exact Commit
    /// can release the already-provisioned reader gate.
    #[must_use]
    pub(crate) fn signal_proof(self, proof: AdoptionProof) -> bool {
        write_wire(&self.fd, &proof.to_wire())
    }
}

/// Why a successor has no Commit it can take: each way its Commit watch
/// ([`CommitReceiver::start_watch`]), or the event loop reading the watch's answer
/// ([`CommitReceiver::receive_commit`]), ends short of this attempt's exact Commit. The
/// fail-stop names it before it leaves ([`fail_stop_without_commit`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoCommit {
    /// The parent's liveness witness ([`AttestedParent::still_alive`]) no
    /// longer names it, and a last look found nothing in the pipe.
    ParentGone,
    /// `poll` answered these `revents` (`POLLHUP`, `POLLERR`, `POLLNVAL`) and
    /// nothing to read.
    PipeEnded { revents: libc::c_short },
    /// `poll` failed with this `errno`.
    PollFailed { errno: i32 },
    /// The read found end of file: every writer closed the pipe and none
    /// wrote a Commit.
    EndOfFile,
    /// The read returned `len` bytes. A Commit is one write of
    /// [`READY_WIRE_LEN`] bytes, so this is a malformed Commit, not a prefix.
    ShortCommit { len: usize },
    /// The read failed with this `errno`.
    ReadFailed { errno: i32 },
    /// A whole Commit arrived, and the event loop no longer holds the
    /// receiver it goes to.
    ReceiverGone,
    /// A whole Commit arrived that does not match this attempt's adoption
    /// proof.
    Mismatched,
    /// The watch ended without handing the event loop an answer.
    NoAnswer,
}

#[cfg(unix)]
impl std::fmt::Display for NoCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let os = |errno: i32| std::io::Error::from_raw_os_error(errno);
        match *self {
            Self::ParentGone => f.write_str(
                "the parent is gone (its liveness witness no longer names it: it exited, or is \
                 exiting) and the pipe held no Commit",
            ),
            Self::PipeEnded { revents } => {
                f.write_str("the pipe answered")?;
                for (bit, name) in [
                    (libc::POLLHUP, " POLLHUP (hang-up)"),
                    (libc::POLLERR, " POLLERR (error)"),
                    (libc::POLLNVAL, " POLLNVAL (invalid descriptor)"),
                ] {
                    if revents & bit != 0 {
                        f.write_str(name)?;
                    }
                }
                f.write_str(" with nothing to read")
            }
            Self::PollFailed { errno } => write!(f, "poll failed: {}", os(errno)),
            Self::EndOfFile => f.write_str(
                "end of file: every writer closed the pipe without a Commit (the parent closed \
                 it, or exited)",
            ),
            Self::ShortCommit { len } => {
                write!(f, "a malformed Commit: {len} bytes of {READY_WIRE_LEN}")
            }
            Self::ReadFailed { errno } => write!(f, "the read failed: {}", os(errno)),
            Self::ReceiverGone => f.write_str(
                "a Commit arrived and the event loop no longer holds the receiver it goes to",
            ),
            Self::Mismatched => {
                f.write_str("a Commit that does not match this attempt's adoption proof")
            }
            Self::NoAnswer => f.write_str("the watch ended without an answer"),
        }
    }
}

/// The fail-stop: a successor that can no longer take a Commit leaves with 74 through `_exit`,
/// so no App or Session destructor runs and the adopted PTYs stay the parent's. It logs why
/// (`end`) first, to the durable log and stderr.
///
/// The watch has already closed the Commit pipe's read end, so a Commit the parent writes
/// meanwhile fails (`EPIPE`; the window ignores `SIGPIPE`, as Rust's runtime leaves it) and the
/// parent takes its `commit_write_failed` rejection, keeping its sessions. The log line costs
/// no Commit, so it may take as long as a log write takes.
#[cfg(unix)]
fn fail_stop_without_commit(parent: AttestedParent, end: NoCommit) -> ! {
    aterm_log::error!(
        "seamless handoff: no Commit from parent {}: {end}; this successor fail-stops with 74",
        parent.pid
    );
    crate::logging::stderr_line!(
        "aterm-gui: seamless handoff: no Commit from parent {}: {end}; this successor \
         fail-stops with 74",
        parent.pid
    );
    crate::crash_signal::clean_exit_now(74)
}

#[cfg(unix)]
pub(crate) struct CommitReceiver {
    fd: Option<std::os::fd::OwnedFd>,
    wire: Option<std::sync::mpsc::Receiver<Result<[u8; READY_WIRE_LEN], NoCommit>>>,
    fail_stop: bool,
    parent: AttestedParent,
}

#[cfg(unix)]
impl CommitReceiver {
    /// Reuse the already-attested identity when the control listener waits for
    /// this exact predecessor to exit after Commit.
    pub(crate) fn parent(&self) -> AttestedParent {
        self.parent
    }

    fn raw(fd: std::os::fd::OwnedFd, fail_stop: bool, parent: AttestedParent) -> Self {
        Self {
            fd: Some(fd),
            wire: None,
            fail_stop,
            parent,
        }
    }

    /// Start the parent-liveness read only after startup has completed every
    /// process-environment mutation. From this point, EOF at any pre-Commit cut
    /// fail-stops the readerless candidate even if the parent itself crashed.
    pub(crate) fn start_watch(mut self) -> Option<Self> {
        let fd = self.fd.take()?;
        let fail_stop = self.fail_stop;
        let parent = self.parent;
        let (send, wire) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("aterm-handoff-parent-watch".to_string())
            .spawn(move || {
                use std::os::fd::AsRawFd as _;
                let received = loop {
                    let mut pollfd = libc::pollfd {
                        fd: fd.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: one initialized pollfd; the short timeout bounds
                    // parent-death detection even if another process inherited a
                    // leaked writer during Darwin's non-atomic pipe+CLOEXEC setup.
                    let polled = unsafe { libc::poll(&mut pollfd, 1, 10) };
                    if polled > 0 {
                        // Data always wins over parent-death detection: Commit's
                        // single <=PIPE_BUF write happens before the parent's
                        // `_exit`, and remains readable after reparenting.
                        if pollfd.revents & libc::POLLIN != 0 {
                            break read_atomic_wire(&fd);
                        }
                        let ended =
                            pollfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL);
                        if ended != 0 {
                            break Err(NoCommit::PipeEnded { revents: ended });
                        }
                    } else if polled < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::Interrupted {
                            break Err(NoCommit::PollFailed {
                                errno: error.raw_os_error().unwrap_or(0),
                            });
                        }
                    }
                    // The fail-stop's liveness witness (`AttestedParent`): edge at the parent's
                    // `exit`, not its reap, immune to pid reuse, and independent of the process
                    // tree, since a successor launchd owns has ppid 1 from birth. One libproc
                    // call per 10 ms tick for the bounded overlap, the cadence the leaked-writer
                    // backstop already needs.
                    if !parent.still_alive() {
                        // Close the write-before-exit race with one final
                        // nonblocking data check before fail-stop.
                        pollfd.revents = 0;
                        let final_poll = unsafe { libc::poll(&mut pollfd, 1, 0) };
                        if final_poll > 0 && pollfd.revents & libc::POLLIN != 0 {
                            break read_atomic_wire(&fd);
                        }
                        break Err(NoCommit::ParentGone);
                    }
                };
                // The watch is over, so the read end closes now, before either fail-stop says
                // why: a read end left open would accept a Commit the parent writes from here
                // on, and the parent would `_exit(0)` on it while this process leaves too.
                // Closed, that write fails and the parent keeps its sessions. A write between
                // the watch's last look and this close still lands; nothing runs between them.
                drop(fd);
                if let Err(end) = received
                    && fail_stop
                {
                    // No exact Commit can arrive now, so this candidate can
                    // never become authoritative.
                    fail_stop_without_commit(parent, end)
                }
                if send.send(received).is_err() && fail_stop {
                    // The event-loop authority disappeared while the parent was
                    // deciding. Remaining alive readerless is never terminal.
                    fail_stop_without_commit(parent, NoCommit::ReceiverGone)
                }
            })
            .ok()?;
        self.wire = Some(wire);
        Some(self)
    }

    #[cfg(test)]
    pub(crate) fn for_test(fd: std::os::fd::OwnedFd) -> Self {
        // Attest the harness's own parent through the real admission path, so fixtures hold
        // production's kind of witness. These receivers are `fail_stop: false` and exercise
        // Commit wire delivery; the death edge is `leaked_commit_writer_cannot_hide_parent_death`.
        // SAFETY: `getppid` is a side-effect-free libc getter.
        let creator = unsafe { libc::getppid() };
        let parent = attest_handoff_parent(creator, None)
            .expect("unit fixtures need a live parent process — run under `cargo test`");
        Self::raw(fd, false, parent)
            .start_watch()
            .expect("test commit watcher")
    }

    /// Wait for the exact attempt-bound Commit. No PTY reader may be attached
    /// before this succeeds; `Err` says why it did not.
    pub(crate) fn receive_commit(&self, expected: AdoptionProof) -> Result<(), NoCommit> {
        match self
            .wire
            .as_ref()
            .expect("commit watch must start before event-loop ownership")
            .recv()
        {
            Ok(Ok(wire)) if expected.commit_wire_matches(&wire) => Ok(()),
            Ok(Ok(_)) => Err(NoCommit::Mismatched),
            Ok(Err(end)) => Err(end),
            Err(std::sync::mpsc::RecvError) => Err(NoCommit::NoAnswer),
        }
    }

    /// A full but mismatched Commit is as terminal as EOF. Production children
    /// fail-stop destructor-free, saying `end`; unit fixtures can observe the
    /// `Err` instead.
    pub(crate) fn fail_stop_if_required(&self, end: NoCommit) {
        if self.fail_stop {
            fail_stop_without_commit(self.parent, end)
        }
    }
}

#[cfg(unix)]
fn read_atomic_wire(fd: &std::os::fd::OwnedFd) -> Result<[u8; READY_WIRE_LEN], NoCommit> {
    use std::os::fd::AsRawFd as _;
    let mut wire = [0u8; READY_WIRE_LEN];
    loop {
        // Commit is emitted by one fixed <=PIPE_BUF write. A short positive read
        // is therefore malformed authority, not a prefix to wait on indefinitely.
        let read = unsafe { libc::read(fd.as_raw_fd(), wire.as_mut_ptr().cast(), wire.len()) };
        match usize::try_from(read) {
            Ok(len) if len == wire.len() => return Ok(wire),
            Ok(0) => return Err(NoCommit::EndOfFile),
            Ok(len) => return Err(NoCommit::ShortCommit { len }),
            Err(_) => {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(NoCommit::ReadFailed {
                    errno: error.raw_os_error().unwrap_or(0),
                });
            }
        }
    }
}

#[cfg(unix)]
fn write_wire(fd: &std::os::fd::OwnedFd, wire: &[u8; READY_WIRE_LEN]) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut offset = 0usize;
    while offset < wire.len() {
        // SAFETY: bounded write from the live suffix of a fixed local buffer.
        let wrote = unsafe {
            libc::write(
                fd.as_raw_fd(),
                wire[offset..].as_ptr().cast(),
                wire.len() - offset,
            )
        };
        if wrote > 0 {
            offset += usize::try_from(wrote).unwrap_or(0);
        } else if wrote < 0
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        {
            continue;
        } else {
            return false;
        }
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(unix)]
pub(crate) struct CommitError;

/// Irreversible parent Commit. Success is uninhabited: the fixed 40-byte wire is
/// far below PIPE_BUF, and the same function immediately `_exit`s after its one
/// atomic write. Callers can handle only the pre-Commit error path; adding work or
/// rollback after a successful Commit is structurally impossible.
#[cfg(unix)]
pub(crate) fn commit_and_exit(
    fd: &std::os::fd::OwnedFd,
    proof: AdoptionProof,
) -> Result<std::convert::Infallible, CommitError> {
    use std::os::fd::AsRawFd as _;
    let wire = proof.to_commit_wire();
    loop {
        // SAFETY: one atomic write from a live fixed buffer to the private pipe.
        let wrote = unsafe { libc::write(fd.as_raw_fd(), wire.as_ptr().cast(), wire.len()) };
        if wrote == isize::try_from(wire.len()).unwrap_or(-1) {
            // ON macOS THIS EXIT ALSO ENDS A LAUNCHD JOB. This process is the
            // process of the `application.com.aterm.aterm.*` job LaunchServices
            // created for the instance, so `_exit` makes launchd tear that job
            // down. On the default out-of-band lane the successor was launched
            // through LaunchServices and owns a job of its own, so nothing is
            // lost. Only on the fork FALLBACK lane (taken for the refusals in
            // `app_update_handoff::out_of_band_lane_refusal`; see "WHY THIS
            // `spawn` IS STILL HERE" in `run_handoff_worker`) is the successor a
            // fork child of ours, which survives as a pid-1 orphan holding this
            // dead job's bootstrap context.
            //
            // This is the protocol's point of no return. `_exit` skips every
            // App/Session destructor that could SIGHUP the handed-off PTYs; the
            // crash marker goes with it, because handing off IS a clean end
            // (`crash_signal::clean_exit_now`).
            crate::crash_signal::clean_exit_now(0)
        }
        if wrote < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(CommitError);
    }
}

/// Off unix the readiness signal's type is uninhabited, so `Option<ReadySignal>` is always
/// `None` and every `Some` arm compiles away. Local empty enums rather than `Infallible`
/// because the shared consumer in `lib.rs` names `ReadySignal::raw_fd` as a function value, and
/// an inherent method cannot be hung on a foreign type.
#[cfg(not(unix))]
pub(crate) enum ReadySignal {}
#[cfg(not(unix))]
pub(crate) enum CommitReceiver {}

#[cfg(not(unix))]
impl ReadySignal {
    /// Unreachable by construction: `match *self {}` is the compiler's own proof
    /// that no value of this type exists to have called it.
    #[must_use]
    pub(crate) fn raw_fd(&self) -> i32 {
        match *self {}
    }
}

/// [`take_ready_fd_from`] over the process environment: the env-shaped entry
/// point the handoff suites drive (`handoff_env::through_process_env`).
#[cfg(all(unix, test))]
pub(crate) fn take_ready_fd(
    nonce: Option<String>,
    layout_digest: Option<[u8; 32]>,
    screen_digest: Option<[u8; 32]>,
    target: Option<HandoffTarget>,
    adopted_fds: &[i32],
) -> Option<ReadySignal> {
    crate::handoff_env::through_process_env(&[ENV_READY_FD], |env| {
        take_ready_fd_from(
            env,
            nonce,
            layout_digest,
            screen_digest,
            target,
            adopted_fds,
        )
    })
}

/// Consume and validate the overlap-handoff readiness fd ([`ENV_READY_FD`]), taking it out of
/// the handoff snapshot at once. Fail-closed: any malformed, dead or tty value returns `None`,
/// so a spoof can only lose the signal (the parked parent times out and resumes). CLOEXEC is
/// re-armed here: it was cleared to survive the staged-swap re-exec, and no further child may
/// inherit the fd or the parent's EOF-based crash detection degrades.
#[cfg(unix)]
pub(crate) fn take_ready_fd_from(
    env: &mut HandoffEnv,
    nonce: Option<String>,
    layout_digest: Option<[u8; 32]>,
    screen_digest: Option<[u8; 32]>,
    target: Option<HandoffTarget>,
    adopted_fds: &[i32],
) -> Option<ReadySignal> {
    // Taken out of the snapshot: a one-shot descriptor handoff must never be
    // consumable twice.
    let raw = env.take(ENV_READY_FD).and_then(|v| v.into_string().ok());
    let fd: i32 = raw?.trim().parse().ok()?;
    if fd < 3 {
        return None; // never adopt stdio
    }
    // Live-fd probe. SAFETY: `F_GETFD` on an arbitrary fd is side-effect-free.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return None;
    }
    // SAFETY: the parent created this fd solely for us; we are its sole owner now.
    let fd = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    use std::os::fd::AsRawFd as _;
    aterm_pty::set_cloexec(fd.as_raw_fd(), true).ok()?;
    // Own before rejecting so every live non-stdio malformed channel closes.
    if adopted_fds.contains(&fd.as_raw_fd()) || aterm_pty::fd_is_tty(fd.as_raw_fd()) {
        return None;
    }
    Some(ReadySignal {
        fd,
        nonce: nonce?,
        layout_digest: layout_digest?,
        screen_digest: screen_digest?,
        target: target?,
    })
}

/// Consume + validate the parent-to-child Commit channel from the handoff
/// snapshot. It is deliberately distinct from the proof/ACK channel so EOF
/// proves the validated parent exited. The last of the channel admissions that
/// make an intake an overlap adoption.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_successor_warm_before_claim",
        action = "Adopt",
        project = "aterm_gui::seamless::handoff_env_conformance::project"
    )
)]
pub(crate) fn take_commit_fd_from(
    env: &mut HandoffEnv,
    nonce: Option<String>,
    adopted_fds: &[i32],
    ready_fd: Option<i32>,
    parent: Option<AttestedParent>,
) -> Option<CommitReceiver> {
    // Taken out of the snapshot: a one-shot descriptor handoff must never be
    // consumable twice.
    let raw = env.take(ENV_COMMIT_FD).and_then(|v| v.into_string().ok());
    let _nonce = nonce?;
    // Holding an `AttestedParent` proves identity; admission asks only whether that process is
    // still there to send a Commit. Startup since `prearm_incoming_fds` can be long (a boot
    // apply re-runs the whole codesign gate).
    let parent = parent?;
    if !parent.still_alive() {
        return None;
    }
    let fd: i32 = raw?.trim().parse().ok()?;
    if fd < 3 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return None;
    }
    // `ready` already owns this alias. Reject without constructing a second
    // OwnedFd (and without closing the first owner's channel).
    if ready_fd == Some(fd) {
        return None;
    }
    // SAFETY: the parent created this fd solely for us; we are its sole owner now.
    let fd = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    use std::os::fd::AsRawFd as _;
    aterm_pty::set_cloexec(fd.as_raw_fd(), true).ok()?;
    if adopted_fds.contains(&fd.as_raw_fd()) || aterm_pty::fd_is_tty(fd.as_raw_fd()) {
        return None;
    }
    Some(CommitReceiver::raw(fd, true, parent))
}

/// Non-unix stub: the overlap handoff is a unix mechanism (the Windows update
/// lane is already spawn-then-exit with no fd handoff).
#[cfg(not(unix))]
pub(crate) fn take_ready_fd_from(
    _env: &mut HandoffEnv,
    _nonce: Option<String>,
    _layout_digest: Option<[u8; 32]>,
    _screen_digest: Option<[u8; 32]>,
    _target: Option<HandoffTarget>,
    _adopted_fds: &[i32],
) -> Option<ReadySignal> {
    None
}

#[cfg(not(unix))]
pub(crate) fn take_commit_fd_from(
    _env: &mut HandoffEnv,
    _nonce: Option<String>,
    _adopted_fds: &[i32],
    _ready_fd: Option<i32>,
    _parent: Option<i32>,
) -> Option<CommitReceiver> {
    None
}

/// The per-record hard refusals of a master this process is about to adopt, shared by both
/// entries into the adoption landing: [`take_incoming_from`]'s handed-off masters and the
/// keeper's recovered ones (`keeper_link::admit_recovered_master`). A master that is not a tty
/// is refused, and so is one whose `FD_CLOEXEC` cannot be re-armed: a descriptor that arrived
/// without it (cleared for the exec, or received over a socket) would leak into every
/// subprocess.
pub(crate) fn admit_master_descriptor(fd: i32) -> Result<(), &'static str> {
    if !aterm_pty::fd_is_tty(fd) {
        return Err("was handed a master that is not a tty");
    }
    if aterm_pty::set_cloexec(fd, true).is_err() {
        return Err("could not re-arm CLOEXEC on its master");
    }
    Ok(())
}

/// The keeper meta's tab stops, packed. The handoff's meta carries each cursor's stops as a
/// JSON bool per column (about six bytes a column), the alternate screen adds a second cursor,
/// and a grid never shrinks the vector, so from about 119 alternate-screen columns (330 on the
/// main screen) even the bare scalars pass the keeper's 4 KiB (`aterm_keeper::wire::MAX_META`).
/// A keeper meta the old form does not fit writes each cursor's `tab_stops` as an empty list and
/// carries the stops under [`PACKED_TAB_STOPS_KEY`]: the vector's length and, only when they are
/// not the every-8 defaults ([`aterm_core::grid::is_default_tab_stop`]), a bitmap of four
/// columns per lower-case hex digit (column `4i + j` is bit `j` of digit `i`), trailing zero
/// digits left off. Default stops cost about thirty bytes at any width, others at most a quarter
/// byte a column more.
///
/// Version skew: the keeper never decodes a meta (the AKP1 wire's `PROTO` versions its frames,
/// not this body) and the body has no version; its one reader is whichever window recovers. A
/// newer window reads an older one's meta as before, and [`keeper_meta_of`] writes the old
/// form, byte for byte, whenever the whole carry fits in it. An older window reading a packed
/// meta ignores the unknown key and reads each cursor's empty list, and its Repaint rung resets
/// that cursor (`repaint_checkpoint`'s sanitize): it keeps every mode and loses the scroll
/// region, margins and custom stops, which a full-screen program sets again as it repaints after
/// the size pulse. Where an older writer would have sent a shed old form (about 105-118
/// alternate-screen columns, or 80 with a pushed colour table), the older reader now keeps the
/// directory, colours and title instead of the region, margins and custom stops
/// (`where_only_a_shed_old_form_fit_the_meta_goes_packed_and_whole`). Packing comes before
/// shedding because a keeper recovery is nearly always the same build relaunched, which gets the
/// whole state here. The handoff's own wire keeps the bool form for its successors, and its meta
/// budget sheds the stops past the width instead (`shed_metas_to_budget`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PackedTabStops {
    /// The stop vector's length: at least the width, and more for a pane once
    /// wider, whose stops past the width a later widen finds again.
    len: u16,
    /// The stops as the bitmap; absent when they are the every-8 defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hex: Option<String>,
}

/// The member of a keeper META's cursor that carries its [`PackedTabStops`].
#[cfg(unix)]
const PACKED_TAB_STOPS_KEY: &str = "tab_stops_packed";

/// The stops a fresh grid `cols` wide holds, one every eighth column
/// ([`aterm_core::grid::is_default_tab_stop`]): what a carried cursor's stops
/// become wherever this module replaces them.
#[cfg(any(unix, test))]
pub(crate) fn default_tab_stops(cols: usize) -> Vec<bool> {
    (0..cols)
        .map(aterm_core::grid::is_default_tab_stop)
        .collect()
}

impl PackedTabStops {
    /// `stops` packed; `None` for a vector longer than a `u16` counts, which
    /// no grid holds.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn pack(stops: &[bool]) -> Option<Self> {
        let len = u16::try_from(stops.len()).ok()?;
        let defaults = stops
            .iter()
            .enumerate()
            .all(|(col, &stop)| stop == aterm_core::grid::is_default_tab_stop(col));
        let hex = (!defaults).then(|| {
            let mut hex = stops
                .chunks(4)
                .map(|nibble| {
                    let bits = nibble
                        .iter()
                        .enumerate()
                        .filter(|(_, stop)| **stop)
                        .fold(0_u32, |bits, (bit, _)| bits | (1 << bit));
                    char::from_digit(bits, 16).unwrap_or('0')
                })
                .collect::<String>();
            hex.truncate(hex.trim_end_matches('0').len());
            hex
        });
        Some(Self { len, hex })
    }

    /// The stop vector, or `None` for a packing no window writes — an empty
    /// or over-wide vector, a bitmap longer than its length or with a digit
    /// that is not hex, or a stop past the length. The caller then keeps the
    /// META's own `tab_stops`, which the Repaint rung judges as it judges any.
    #[cfg(any(unix, test))]
    fn unpack(&self) -> Option<Vec<bool>> {
        let len = usize::from(self.len);
        if len == 0 || len > usize::from(aterm_core::grid::MAX_GRID_COLS) {
            return None;
        }
        let Some(hex) = &self.hex else {
            return Some(default_tab_stops(len));
        };
        if hex.len() > len.div_ceil(4) {
            return None;
        }
        let mut stops = vec![false; len];
        for (digit, ch) in hex.chars().enumerate() {
            let bits = ch.to_digit(16)?;
            for bit in 0..4 {
                if bits & (1 << bit) != 0 {
                    *stops.get_mut(digit * 4 + bit)? = true;
                }
            }
        }
        Some(stops)
    }
}

/// [`keeper_meta_of`] of a terminal's meta carry, for tests: the pump takes
/// the carry under the terminal's lock and builds the META after releasing it.
#[cfg(all(unix, test))]
pub(crate) fn keeper_meta(term: &aterm_core::terminal::Terminal) -> Option<Vec<u8>> {
    keeper_meta_of(term.carry_meta_abandoning_partial(), None)
}

/// The top-level member of a keeper META that carries the session's fabric
/// ([`crate::fabric::KeeperCarry`]). A window before it ignores the member, as
/// it ignores [`PACKED_TAB_STOPS_KEY`].
#[cfg(unix)]
const KEEPER_FABRIC_KEY: &str = "fabric";

/// `json`, a META object, with `fabric` as its [`KEEPER_FABRIC_KEY`] member,
/// appended so the screen's own bytes stay as they were.
#[cfg(unix)]
fn with_fabric(mut json: Vec<u8>, fabric: Option<&[u8]>) -> Option<Vec<u8>> {
    let Some(fabric) = fabric else {
        return Some(json);
    };
    if json.pop() != Some(b'}') {
        return None;
    }
    if json != b"{" {
        json.push(b',');
    }
    json.push(b'"');
    json.extend_from_slice(KEEPER_FABRIC_KEY.as_bytes());
    json.extend_from_slice(b"\":");
    json.extend_from_slice(fabric);
    json.push(b'}');
    Some(json)
}

/// The keeper's META: the Repaint rung's scalar state of a live terminal (modes, keyboard
/// protocols, charsets, the pen, the cursor, the directory and title) as the JSON the
/// handoff's screen carry uses, without either grid, without the shell-integration nonce (the
/// keeper holds no secret; the re-key channel heals it), and with no partial escape sequence
/// or in-flight output state (`CheckpointMeta::output_state`: the REP character, an open OSC 8
/// link, a VT52 address in progress). The keeper link resends this only when the modes change,
/// so at a crash it may be hours old: a link open then would link everything printed after the
/// recovery, and a link's URI (up to 8 KiB) alone could push the META past its cap.
///
/// At most [`aterm_keeper::wire::MAX_META`] bytes. The old (bool-list) form goes when the whole
/// carry fits it ([`PackedTabStops`] has the version skew); else the tab stops travel packed,
/// so the size no longer grows with the width, and what still does not fit is shed one rung at
/// a time, least important first: the stops past the width; the directory and colours; the
/// title and mark state; and last the custom stops, the every-8 defaults standing in. What is
/// left is a fixed set of scalars: on a 50-row session a packed META is about 2.2 KB on the
/// main screen and 2.8 KB on the alternate one at any width (the old form reaches 50 KB at
/// 4,096 columns), and the widest session `a_meta_is_built_for_every_session` builds sheds to
/// 3,675 bytes, 421 under the cap. So `None` means only that those scalars outgrew the cap.
///
/// THE SESSION'S FABRIC rides beside them, under [`KEEPER_FABRIC_KEY`]
/// ([`crate::fabric::KeeperCarry`]: the holds, the id lease, the topics, the
/// queued post ids, the owed receipts' count and `dropped=`), in every rung: what does
/// not fit is the screen's to shed first, since a lost topic is lost consent
/// and a lost hold a lifted halt, and none of the screen's rungs is more than
/// what a repaint or a `cd` restores. Only when even the fewest scalars leave
/// no room do the fabric's lists give way to their counts, and then the
/// holds' reasons to a fixed word (its `rungs`) — never a hold. Measured
/// 2026-10-16 with the fullest fabric, both origins held with the longest
/// reasons JSON can double and the lease and `dropped=` at their ceiling:
/// 3,975 bytes for the fullest session, every reason whole; with the widest
/// scalars as well, 3,892, the reasons given way and both holds standing. `None` for `fabric` is a test's META, the
/// screen alone.
#[cfg(unix)]
pub(crate) fn keeper_meta_of(
    mut meta: CheckpointMeta,
    fabric: Option<&crate::fabric::KeeperCarry>,
) -> Option<Vec<u8>> {
    meta.shell_integration_nonce = None;
    meta.parser = None;
    meta.output_state = aterm_core::terminal::OutputStateRepr::default();
    let Some(fabric) = fabric else {
        return keeper_meta_shed(meta, None);
    };
    fabric.rungs().iter().find_map(|rung| {
        let section = aterm_json::to_vec(rung).ok()?;
        keeper_meta_shed(meta.clone(), Some(&section))
    })
}

/// [`keeper_meta_of`]'s screen: the whole carry in the old form, else packed
/// and shed one rung at a time, each with `fabric` beside it.
#[cfg(unix)]
fn keeper_meta_shed(mut meta: CheckpointMeta, fabric: Option<&[u8]>) -> Option<Vec<u8>> {
    fn cursors(
        meta: &mut CheckpointMeta,
    ) -> impl Iterator<Item = &mut aterm_core::terminal::GridCursorRepr> {
        std::iter::once(&mut meta.cursor).chain(&mut meta.alt_cursor)
    }
    let under_cap = |bytes: Vec<u8>| {
        with_fabric(bytes, fabric).filter(|bytes| bytes.len() <= aterm_keeper::wire::MAX_META)
    };
    if let Some(bytes) = aterm_json::to_string(&meta)
        .ok()
        .map(String::into_bytes)
        .and_then(under_cap)
    {
        return Some(bytes);
    }
    let shed: [fn(&mut CheckpointMeta); 4] = [
        |meta| {
            let cols = usize::from(meta.cols);
            for cursor in cursors(meta) {
                cursor.tab_stops.truncate(cols);
            }
        },
        |meta| {
            meta.current_working_directory = None;
            meta.color = aterm_core::terminal::ColorRepr::default();
        },
        |meta| {
            meta.title = None;
            meta.shell = aterm_core::terminal::ShellRepr::default();
        },
        |meta| {
            let cols = usize::from(meta.cols);
            for cursor in cursors(meta) {
                cursor.tab_stops = default_tab_stops(cols);
                cursor.tab_defaults_suppressed = false;
            }
        },
    ];
    for rung in shed {
        if let Some(bytes) = keeper_meta_json(meta.clone()).and_then(under_cap) {
            return Some(bytes);
        }
        rung(&mut meta);
    }
    keeper_meta_json(meta).and_then(under_cap)
}

/// `meta` as the keeper's META: the handoff's meta JSON with each cursor's
/// stops packed ([`PackedTabStops`]) beside an empty `tab_stops`.
#[cfg(unix)]
fn keeper_meta_json(mut meta: CheckpointMeta) -> Option<Vec<u8>> {
    let packed = [
        PackedTabStops::pack(&std::mem::take(&mut meta.cursor.tab_stops)),
        meta.alt_cursor
            .as_mut()
            .and_then(|alt| PackedTabStops::pack(&std::mem::take(&mut alt.tab_stops))),
    ];
    let mut value = aterm_json::to_value(&meta).ok()?;
    let object = value.as_object_mut()?;
    for (slot, packed) in ["cursor", "alt_cursor"].into_iter().zip(packed) {
        let (Some(packed), Some(cursor)) = (
            packed,
            object
                .get_mut(slot)
                .and_then(aterm_json::Value::as_object_mut),
        ) else {
            continue;
        };
        cursor.insert(
            PACKED_TAB_STOPS_KEY.to_owned(),
            aterm_json::to_value(&packed).ok()?,
        );
    }
    aterm_json::to_vec(&value).ok()
}

/// The keeper's Repaint rung: the checkpoint a keeper-recovered session is adopted onto, a
/// blank canonical screen carrying the META the dead window last sent ([`keeper_meta_of`]),
/// built by the degrade a refused handoff screen takes ([`repaint_checkpoint`]) and priced
/// against `used_cells` across the recovery. `None` for a META that is empty or does not read
/// as the handoff's meta (the session then lands on a blank engine and is re-keyed). Never a
/// nonce, partial sequence or in-flight output state from it, whatever window sent it: the
/// META is a sample from before the crash, not the moment of it.
#[cfg(unix)]
pub(crate) fn recovered_checkpoint(
    meta: &[u8],
    used_cells: &mut u64,
) -> Option<TerminalCheckpoint> {
    let text = std::str::from_utf8(meta).ok()?;
    let value: aterm_json::Value = aterm_json::from_str(text).ok()?;
    let object = value.as_object()?;
    if !CHECKPOINT_META_REQUIRED_KEYS
        .iter()
        .all(|key| object.contains_key(*key))
    {
        return None;
    }
    // A newer META's packed tab stops ([`PackedTabStops`]); an older one has
    // none and its bool lists read as they always did.
    let packed = |slot: &str| {
        let packed = object.get(slot)?.get(PACKED_TAB_STOPS_KEY)?.clone();
        aterm_json::from_value::<PackedTabStops>(packed)
            .ok()?
            .unpack()
    };
    let (main_stops, alt_stops) = (packed("cursor"), packed("alt_cursor"));
    let mut meta: CheckpointMeta = aterm_json::from_value(value).ok()?;
    if let Some(stops) = main_stops {
        meta.cursor.tab_stops = stops;
    }
    if let (Some(stops), Some(alt)) = (alt_stops, meta.alt_cursor.as_mut()) {
        alt.tab_stops = stops;
    }
    meta.shell_integration_nonce = None;
    meta.parser = None;
    meta.output_state = aterm_core::terminal::OutputStateRepr::default();
    Some(
        repaint_checkpoint(Some(&meta), used_cells)
            .unwrap_or_else(|| unadmitted_blank_carry(&meta)),
    )
}

/// What a keeper-recovered session's fabric comes back as
/// ([`recovered_fabric`]): the rows and the carry `spawn::seed_adopted_fabric`
/// puts back, and what the META counted without naming.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct RecoveredFabric {
    /// The hold rows, the META's `hold` and then its `more_holds`, read
    /// fail-closed by `fabric::parse_holds`.
    pub(crate) holds: Vec<String>,
    /// The topic rows, validated by `fabric::parse_topics`.
    pub(crate) topics: Vec<String>,
    /// The id lease (or the fence), the queued post ids to retire dead and
    /// the owed receipts' count.
    pub(crate) carry: crate::fabric::FabricCarry,
    /// Opt-ins the META had no room to name.
    pub(crate) topics_lost: u64,
    /// Queued posts the META had no room to name.
    pub(crate) queued_lost: u64,
}

/// THE FABRIC A KEEPER META CARRIED ([`crate::fabric::KeeperCarry`], under
/// [`KEEPER_FABRIC_KEY`]), read LENIENTLY and apart from the screen, so a META
/// whose screen does not read still gives its hold back. The META is another
/// window's word, possibly another build's:
///
/// * no section (an older window's META, or none at all): no hold and no
///   topics, as before, and the id counters FENCED
///   ([`crate::fabric::FabricCarry::fenced`]) rather than restarted;
/// * a `hold` that is not a string, a `more_holds` member that is not one or
///   a `more_holds` that is no list, or a section that is not an object: a
///   row outside the grammar, which `parse_hold` turns into a local
///   `carried-unreadable` halt — dropping it could lift one;
/// * a META from before the two slots carries `hold` alone, which is that
///   one hold;
/// * a lease that is not three counters: fenced; a list member that is not
///   its type: skipped (the topic rows are validated where they are seeded,
///   and a queued id the lease does not cover is never retired); a count that
///   is not one (`owed`, `dropped`, `topics_lost`, `queued_lost`): 0.
#[cfg(unix)]
pub(crate) fn recovered_fabric(meta: &[u8]) -> RecoveredFabric {
    use aterm_json::Value;
    let value = std::str::from_utf8(meta)
        .ok()
        .and_then(|text| aterm_json::from_str::<Value>(text).ok());
    let Some(section) = value
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|object| object.get(KEEPER_FABRIC_KEY))
    else {
        return RecoveredFabric {
            carry: crate::fabric::FabricCarry::fenced(),
            ..RecoveredFabric::default()
        };
    };
    let Some(section) = section.as_object() else {
        return RecoveredFabric {
            holds: vec![String::new()],
            carry: crate::fabric::FabricCarry::fenced(),
            ..RecoveredFabric::default()
        };
    };
    let count = |key: &str| section.get(key).and_then(Value::as_u64).unwrap_or(0);
    let list = |key: &str| section.get(key).and_then(Value::as_array);
    let lease = list("ids")
        .and_then(|ids| ids.iter().map(Value::as_u64).collect::<Option<Vec<u64>>>())
        .and_then(|ids| <[u64; 3]>::try_from(ids).ok());
    let mut carry = lease.map_or_else(
        crate::fabric::FabricCarry::fenced,
        crate::fabric::FabricCarry::leased,
    );
    carry.uncarried = list("queued")
        .map(|ids| ids.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    carry.receipts_uncarried = count("owed");
    // `dropped=` goes on counting from the dead window's; a META from before
    // the count reads 0, as it always did.
    carry.dropped = count("dropped");
    // Fail closed, member by member: what is not a string is an unreadable
    // row, and a `more_holds` that is no list is one.
    let row = |hold: &Value| hold.as_str().unwrap_or_default().to_owned();
    let mut holds: Vec<String> = section.get("hold").map(row).into_iter().collect();
    match section.get("more_holds") {
        None => {}
        Some(Value::Array(rows)) => holds.extend(rows.iter().map(row)),
        Some(_) => holds.push(String::new()),
    }
    RecoveredFabric {
        holds,
        topics: list("topics")
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        carry,
        topics_lost: count("topics_lost"),
        queued_lost: count("queued_lost"),
    }
}

/// [`take_incoming_from`] over the process environment: the env-shaped entry
/// point the handoff suites drive (`handoff_env::through_process_env`).
#[cfg(all(test, unix))]
pub(crate) fn take_incoming() -> IncomingHandoff {
    take_incoming_as(ReceiverShape::Current)
}

/// Authenticate and consume the incoming handoff from the handoff snapshot `main_entry`
/// captured, returning the [`Adopted`] live sessions (empty: no seamless handoff, or it failed
/// auth; start fresh). Consumes the four names from `env` and deletes the manifest and screen
/// sidecars (single-use), so a stale manifest can never be replayed. The launched lane's
/// granted descriptors are in `env` too (`ClaimedIntake::install`), so the same manifest join,
/// bijection, tty backstop, digests and parent birth run over the same strings.
///
/// The control carry rides along best-effort: each record's `.ctl` sidecar is read and deleted
/// at its exact path, checked against the record's `control` stamp and decoded onto
/// [`Adopted::control`], and the manifest's `next_turn_id` continues this process's turn-id
/// count before any session spawns. Neither can make this return an empty handoff.
///
/// This is the newer build reading the older build's bytes, so hostile input is checked here,
/// and only this side's rules can be corrected for the hop. It refuses the whole handoff only
/// for identity and resource facts: the env/nonce/path binding, the manifest's parse or size, a
/// session it was not told of, a master that is not a tty or cannot be made CLOEXEC, a missing
/// screen carry or one naming another record's sidecar, a sidecar over its byte cap, the
/// aggregate byte cap, and more sessions than the protocol carries. A screen whose content it
/// refuses (meta, dimensions, canonicality, alt presence) and a layout it cannot parse cost only
/// that screen or layout: the shell is adopted onto a blank engine and repainted
/// (`Adopted::repaint`), or placed as a tab, with one WARN line naming the session and why, and
/// both digests are still taken over the bytes as read, so the proof matches every parent in
/// the field.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_successor_warm_before_claim",
        action = "TakeOwned",
        project = "aterm_gui::seamless::handoff_env_conformance::project"
    )
)]
pub(crate) fn take_incoming_from(env: &mut HandoffEnv) -> IncomingHandoff {
    take_incoming_from_as(env, ReceiverShape::Current)
}

/// Which manifest shape [`take_incoming_from_as`] reads with. Production is always
/// [`ReceiverShape::Current`]; tests also play a receiver built before the control carry (the
/// older build a rollback hands off to), whose manifest types have no `control` or
/// `next_turn_id` and which never opens a `.ctl`, so a new sender's handoff to it is exercised
/// end to end.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiverShape {
    Current,
    #[cfg(all(test, unix))]
    PreCarry,
}

impl ReceiverShape {
    fn parse(self, toml: &str) -> Option<SessionHandoff> {
        match self {
            Self::Current => SessionHandoff::from_toml(toml),
            #[cfg(all(test, unix))]
            Self::PreCarry => carry_tests::pre_carry_parse(toml),
        }
    }
}

/// Log a whole-handoff refusal and return the empty handoff carrying `whose`. A launched
/// successor has no stderr, so the log is where the reason must go.
fn refused_whole(whose: WholeRefusal, why: &str) -> IncomingHandoff {
    aterm_log::error!(
        "overlap handoff: refusing the incoming handoff whole — {why}; no session is adopted, \
         so the outgoing process keeps every one"
    );
    IncomingHandoff {
        refusal: Some(whose),
        ..IncomingHandoff::default()
    }
}

/// The names [`take_incoming_from_as`] reads: the four it consumes and the two
/// channel names whose presence it checks.
#[cfg(all(test, unix))]
const INTAKE_KEYS: &[HandoffName] = &[
    ENV_MANIFEST,
    ENV_NONCE,
    ENV_FDS,
    ENV_LAYOUT,
    ENV_READY_FD,
    ENV_COMMIT_FD,
];

#[cfg(all(test, unix))]
fn take_incoming_as(shape: ReceiverShape) -> IncomingHandoff {
    crate::handoff_env::through_process_env(INTAKE_KEYS, |env| take_incoming_from_as(env, shape))
}

/// v0.98.0's build: the first release that reports a withheld carry per record
/// (`SessionRecord::history_withheld`, `RELEASES.ledger`). A producer from it on that follows a
/// no-scrollback policy always says so, so its record without the word means the loss was not
/// the policy's: it read no policy at its park, could not parse it, or its carry failed.
pub(crate) const FIRST_WITHHELD_SAYING_BUILD: u64 = 1_790_607_299;

/// Whether a producer of `build` follows a successor's handoff policy but
/// cannot say per record that it did: v0.97.0, and only it among the releases
/// (`[FIRST_POLICY_FOLLOWING_BUILD, FIRST_WITHHELD_SAYING_BUILD)`). A producer
/// older than v0.97.0's build
/// ([`FIRST_POLICY_FOLLOWING_BUILD`](aterm_update_core::handoff_policy::FIRST_POLICY_FOLLOWING_BUILD))
/// follows none, so a loss in its manifest is its own, whatever a policy asked.
const fn follows_policy_without_saying_so(build: u64) -> bool {
    use aterm_update_core::handoff_policy::FIRST_POLICY_FOLLOWING_BUILD;
    build >= FIRST_POLICY_FOLLOWING_BUILD && build < FIRST_WITHHELD_SAYING_BUILD
}

/// THE SUCCESSOR'S OWN REQUEST, read back (round eight of the update audit,
/// finding 87): whether the handoff policy THIS build's bundle seals asked the
/// producer that wrote `outgoing_build` to carry no scrollback (`carry =
/// "visible"` or `"repaint"`, `aterm_update_core::handoff_policy`), for a
/// producer that would follow it and could not say so.
///
/// Only that producer: one before v0.97.0 followed no policy and one from v0.98.0 on says what
/// it withheld, so for either, reading an unexplained loss as the request would hide a real
/// carry failure. A manifest with no `outgoing_build` is older than the policy. The build is
/// compared with the policy's `applies_to_producers` range, as the producer did.
fn own_policy_withholds_scrollback(outgoing_build: Option<u64>) -> bool {
    outgoing_build.is_some_and(|build| {
        follows_policy_without_saying_so(build)
            && own_handoff_policy().is_some_and(|policy| {
                policy.applies_to(build) && !policy.carry_ceiling().carries_scrollback()
            })
    })
}

/// The handoff policy this build's own bundle seals as v0.97.0 read it, or `None`: no bundle (a
/// dev build, another platform), no file, or one this reader cannot follow, which the producer
/// reading the same bytes could not have followed either. v0.97.0, the one producer
/// [`own_policy_withholds_scrollback`] asks about, reads the top-level keys alone (`ranges` is
/// unknown to it), so the answer is those keys: beside `[[ranges]]` their hull, never the rescue
/// the list names for that build (`aterm_update_core::handoff_policy::PolicyFile::legacy`).
#[cfg(not(test))]
fn own_handoff_policy() -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe().ok()?;
        let root = crate::app_update_handoff::app_bundle_root(&exe)?;
        match aterm_update_core::handoff_policy::read_from_bundle(&root) {
            aterm_update_core::handoff_policy::PolicyRead::Parsed(file) => Some(file.legacy),
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
thread_local! {
    /// The policy a test plays as this build's own ([`own_handoff_policy`]).
    static OWN_POLICY_FOR_TEST: std::cell::Cell<
        Option<aterm_update_core::handoff_policy::HandoffPolicy>,
    > = const { std::cell::Cell::new(None) };
}

/// [`own_handoff_policy`] in a test: what the test set on its thread, never
/// the test binary's own bundle.
#[cfg(test)]
fn own_handoff_policy() -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
    OWN_POLICY_FOR_TEST.with(std::cell::Cell::get)
}

/// Play `policy` as this build's own sealed handoff policy on this thread, for
/// the rest of the guard's life.
#[cfg(all(test, unix))]
pub(crate) fn own_policy_for_test(
    policy: Option<aterm_update_core::handoff_policy::HandoffPolicy>,
) -> impl Drop {
    struct Restore(Option<aterm_update_core::handoff_policy::HandoffPolicy>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OWN_POLICY_FOR_TEST.with(|cell| cell.set(self.0));
        }
    }
    Restore(OWN_POLICY_FOR_TEST.with(|cell| cell.replace(policy)))
}

/// [`take_incoming_from`] as a receiver of `shape` reads the handoff.
fn take_incoming_from_as(env: &mut HandoffEnv, shape: ReceiverShape) -> IncomingHandoff {
    let ready_env_present = env.present(ENV_READY_FD);
    let commit_env_present = env.present(ENV_COMMIT_FD);
    let manifest_path = env.string(ENV_MANIFEST);
    let env_nonce = env.string(ENV_NONCE);
    let fds_wire = env.string(ENV_FDS);
    let layout_path = env.string(ENV_LAYOUT);
    #[cfg(unix)]
    let mut incoming_pty_guard = fds_wire
        .as_deref()
        .map_or_else(|| IncomingPtyGuard(Vec::new()), IncomingPtyGuard::from_wire);
    // Consumed at once, whatever the outcome: single-use authority. (They left
    // the process environment at `HandoffEnv::capture`, before any thread.)
    for key in [ENV_MANIFEST, ENV_FDS, ENV_NONCE, ENV_LAYOUT] {
        env.remove(key);
    }
    let offered = manifest_path.is_some() || env_nonce.is_some() || fds_wire.is_some();
    let (Some(path), Some(env_nonce), Some(fds_wire)) = (manifest_path, env_nonce, fds_wire) else {
        // No handoff at all is the ordinary cold boot, and says nothing; part
        // of one is a refusal, and says which.
        if offered {
            return refused_whole(
                WholeRefusal::Producer,
                "only part of its environment arrived (ATERM_SEAMLESS_MANIFEST, _NONCE and \
                 _FDS go together)",
            );
        }
        return IncomingHandoff::default();
    };
    let Some(fds) = decode_fds_bounded(&fds_wire) else {
        return refused_whole(
            WholeRefusal::Producer,
            "its descriptor list (ATERM_SEAMLESS_FDS) is malformed",
        );
    };
    // The manifest must live inside our own 0700 control dir: a path elsewhere is a spoof
    // attempt (a different-uid peer cannot write our dir).
    let Some(dir) = crate::control_auth::socket_dir() else {
        return refused_whole(
            WholeRefusal::Producer,
            "there is no private control directory to read its manifest from",
        );
    };
    if !std::path::Path::new(&path).starts_with(&dir)
        || !manifest_path_matches_nonce(std::path::Path::new(&path), &env_nonce)
    {
        return refused_whole(
            WholeRefusal::Producer,
            "its manifest path is outside the control directory or does not match its nonce",
        );
    }
    let Some(body) = take_regular_capped(
        std::path::Path::new(&path),
        &dir,
        MAX_INCOMING_MANIFEST_BYTES,
    )
    .and_then(|bytes| String::from_utf8(bytes).ok()) else {
        return refused_whole(
            WholeRefusal::Producer,
            "its manifest could not be read (missing, not a regular file, over the size cap, or not UTF-8)",
        );
    };
    // First line binds the file to the env nonce (written together by us); mismatch ⇒ a
    // stale/foreign file → reject.
    let (file_nonce, toml) = body.split_once('\n').unwrap_or(("", ""));
    if file_nonce != env_nonce {
        return refused_whole(
            WholeRefusal::Producer,
            "its manifest names a different attempt than ATERM_SEAMLESS_NONCE",
        );
    }
    let Some(manifest) = shape.parse(toml) else {
        return refused_whole(
            WholeRefusal::Own,
            "its manifest does not parse in this build",
        );
    };
    let Some(expected) = validated_identities(&manifest, &fds) else {
        return refused_whole(
            WholeRefusal::Producer,
            "its manifest's sessions do not match the descriptors handed over",
        );
    };
    let expected_ids = expected
        .iter()
        .map(|(local_id, _, _)| *local_id)
        .collect::<Vec<_>>();
    // Modern overlap only: every parent that can hand off to this binary sends a Commit
    // channel, a readiness channel and an attempt-bound layout sidecar.
    if !(commit_env_present && ready_env_present && layout_path.is_some()) {
        return refused_whole(
            WholeRefusal::Producer,
            "its readiness channel, Commit channel or layout sidecar variable is missing",
        );
    }
    // The layout degrades, it does not refuse. The sidecar must still be read at its exact path
    // and hashed: a missing, oversized or non-UTF-8 sidecar is not what the parent wrote, and
    // without its bytes there is no layout digest to prove. A wire this build cannot parse, or
    // whose terminal leaves do not name exactly the authenticated sessions, is content: every
    // session is still adopted and placed as a tab of the first window by the orphan net
    // (`App::adopt_orphan_shells_as_tabs`), and the digest over the bytes read still matches.
    let Some((layout, layout_digest, unplaced_layout, unplaced_drafts)) =
        layout_path.and_then(|layout_path| {
            let layout_path = std::path::Path::new(&layout_path);
            let expected_layout = std::path::Path::new(&path).with_extension("layout.toml");
            if !layout_path.starts_with(&dir) || layout_path != expected_layout {
                return None;
            }
            let wire = take_regular_capped(layout_path, &dir, MAX_HANDOFF_LAYOUT_BYTES)
                .and_then(|bytes| String::from_utf8(bytes).ok())?;
            // Commit to the bytes, not the parse: `layout` only rebuilds panes, and the digest
            // the parent compares is a pure function of this exact wire.
            let digest = layout_wire_digest(&wire)?;
            // A leaf the outgoing process handed no session for (an exited pane
            // kept open under `--hold`) is placed as a placeholder, not a reason
            // to drop the whole layout (`RestoreManifest::placed_for_handed`).
            Some(match crate::restore::RestoreManifest::from_toml(&wire) {
                Some(layout) => match layout.placed_for_handed(&expected_ids) {
                    Ok(layout) => (Some(layout), digest, None, (Vec::new(), 0)),
                    Err(unplaced) => (
                        None,
                        digest,
                        Some("does not name exactly the handed-off sessions"),
                        unplaced.carried_settings_drafts(),
                    ),
                },
                None => (
                    None,
                    digest,
                    Some("does not parse in this build"),
                    (Vec::new(), 0),
                ),
            })
        })
    else {
        return refused_whole(
            WholeRefusal::Producer,
            "its layout sidecar is missing, misplaced, over the size cap or not UTF-8",
        );
    };
    let manifest_path = std::path::Path::new(&path);
    let Some(manifest_stem) = manifest_path.file_stem().and_then(|stem| stem.to_str()) else {
        return refused_whole(
            WholeRefusal::Producer,
            "its manifest path has no usable file stem",
        );
    };
    let mut remaining_grid_bytes = MAX_HANDOFF_AGGREGATE_GRID_BYTES;
    let mut used_grid_cells = 0_u64;
    let mut remaining_control_bytes = crate::handoff_carry::MAX_AGGREGATE_BYTES;
    let mut remaining_history_bytes = crate::handoff_history::MAX_AGGREGATE_BYTES;
    // Read at most once, and only for a record whose loss it could explain.
    // A producer that followed a no-scrollback policy exported nothing, so a
    // manifest that names a scrollback sidecar for any record was not parked
    // under one: none of its losses is the policy's.
    let mut asked_for_no_scrollback: Option<bool> = manifest
        .sessions
        .iter()
        .any(|rec| rec.history.is_some())
        .then_some(false);
    let incoming = manifest
        .sessions
        .iter()
        .map(|rec| {
            // Hard refusals first: identity, descriptor and resource facts reject the whole
            // adoption before any content is judged, since they make the PTY this process's to
            // own and bound what adopting it may allocate. `validated_identities` made the
            // record and fd sets one bijection.
            let (_, fd, pid) = expected
                .iter()
                .find(|(local_id, _, _)| *local_id == rec.local_id)
                .copied()?;
            // The per-record hard refusals, shared with the PTY keeper's
            // recovery (`admit_master_descriptor`): a master that is not a tty,
            // or whose CLOEXEC cannot be re-armed (the outgoing parent clears
            // it only in this process's pre-exec child image; left clear, a
            // later subprocess would inherit the PTY master).
            if let Err(why) = admit_master_descriptor(fd) {
                return refuse_incoming_session(rec.local_id, why);
            }
            // The screen carry is mandatory and exact-path bound: every producer sends one, so
            // a record without it, or naming any sidecar but its own, is not one to read.
            let Some(sc) = rec.screen.as_ref() else {
                return refuse_incoming_session(rec.local_id, "carried no screen");
            };
            let meta = parse_checkpoint_meta(sc);
            // A loss this build asked for: a record that counts lines left behind, names no
            // sidecar and carried no line in its screen (all a v0.97.0 producer following a
            // no-scrollback policy can say) is the policy's withholding when this build's own
            // sealed policy asked that producer for no scrollback and no record names a
            // sidecar.
            let followed_no_scrollback = rec.history_dropped > 0
                && !rec.history_withheld
                && rec.history.is_none()
                && (sc.repaint || meta.as_ref().is_some_and(|meta| meta.history_lines == 0))
                && *asked_for_no_scrollback.get_or_insert_with(|| {
                    own_policy_withholds_scrollback(manifest.outgoing_build)
                });
            let wire = match take_screen_wire(
                sc,
                meta.as_ref(),
                &manifest_path.with_file_name(format!("{manifest_stem}.s{}.grid", rec.local_id)),
                &manifest_path.with_file_name(format!("{manifest_stem}.s{}.altgrid", rec.local_id)),
                &dir,
                &mut remaining_grid_bytes,
            ) {
                Ok(wire) => wire,
                Err(why) => return refuse_incoming_session(rec.local_id, why),
            };
            // Then the content, which costs at most this session's screen.
            let screen = admit_incoming_screen(meta, wire, &mut used_grid_cells);
            let repaint = sc.repaint || matches!(screen, IncomingScreen::Degraded { .. });
            // The control carry, best-effort: whatever happens to it, this session adopts. A
            // repainted session goes without its differ's state (the archive would diff the
            // program's next frame against a screen that was not carried) but keeps its ledger
            // and archive rows.
            let control_path =
                manifest_path.with_file_name(format!("{manifest_stem}.s{}.ctl", rec.local_id));
            let control = match shape {
                ReceiverShape::Current => rec.control.as_deref().and_then(|stamp| {
                    let mut control = take_control_sidecar(
                        &control_path,
                        &dir,
                        stamp,
                        &mut remaining_control_bytes,
                    )
                    // A lost carry is logged: the session's `history` starts empty and its
                    // archive afresh, which reads like a session nobody drove unless the log
                    // names the loss.
                    .inspect_err(|cause| {
                        aterm_log::warn!(
                            "overlap handoff: session {} adopts without its control carry \
                             (turn ledger, archive tail): {cause}",
                            rec.local_id
                        );
                    })
                    .ok()?;
                    if repaint {
                        control.without_differ();
                    }
                    Some(control)
                }),
                #[cfg(all(test, unix))]
                ReceiverShape::PreCarry => None,
            };
            // The history carry (`crate::handoff_history`), best-effort like the control carry:
            // its sidecar is opened and unlinked here, before the proof (the outgoing process
            // retires what is left once the proof checks out), and read after Commit. A sidecar
            // that cannot be opened, or whose screen this build refused, costs exactly its
            // lines, counted onto the session's `history_lost`.
            let history_path =
                manifest_path.with_file_name(format!("{manifest_stem}.s{}.hist", rec.local_id));
            let history = match shape {
                ReceiverShape::Current => crate::handoff_history::incoming(
                    rec,
                    matches!(screen, IncomingScreen::Degraded { .. }),
                    followed_no_scrollback,
                    &history_path,
                    &dir,
                    &mut remaining_history_bytes,
                ),
                #[cfg(all(test, unix))]
                ReceiverShape::PreCarry => crate::handoff_history::AdoptedHistory::default(),
            };
            let (checkpoint, degraded) = match screen {
                IncomingScreen::Exact(checkpoint) => (Some(checkpoint), None),
                IncomingScreen::Degraded {
                    checkpoint,
                    cause,
                    wire,
                } => (checkpoint, Some((cause, wire))),
            };
            // THE RENDER DEBT (R08): the rows and columns the parent's engine
            // owed this session's alternate screen. Seeded only onto a screen that was
            // carried (a blank one, repainted by the program, has none) and that
            // restores onto the alternate screen; a value that does not read is
            // no debt. Never in either proof digest, never a reason to refuse.
            let render_debt = rec.carried_render_debt().filter(|_| {
                !repaint
                    && checkpoint
                        .as_ref()
                        .is_some_and(|cp| cp.modes.alternate_screen)
            });
            // Ownership of the master moves from the guard to this adoption: from
            // here its `Adopted` closes it if the handoff is refused after all.
            #[cfg(unix)]
            let master = incoming_pty_guard.claim(fd);
            #[cfg(not(unix))]
            let master = crate::spawn::HandedMaster::new(fd);
            Some(IncomingSession {
                degraded,
                adopted: Adopted {
                    // Carry the outgoing pool id so the boot re-adopts this shell into its
                    // original pane (the restore manifest's leaf carries the same id).
                    local_id: rec.local_id,
                    master,
                    pid,
                    // Preserve the fabric SID so `aterm ctl @<sid>` still resolves the session
                    // after the update, under a fresh nonce: edge rows bound to the old nonce
                    // die with the old process. Once every adopted session is registered,
                    // `App::remint_carried_connections` re-mints the manifest's tokenless
                    // `connections` through the one kind-bounded helper under this nonce
                    // (origin `handoff`), so no token or nonce rides the manifest.
                    sid: SessionId::new(rec.sid.clone()),
                    nonce: LaunchNonce::generate(),
                    checkpoint,
                    control,
                    repaint,
                    // Frozen path: the sender wrote no `outgoing_build` (the one build before
                    // the field, whose sessions had no `agents/` in front; presence is the
                    // whole test), or the sender adopted this shell frozen and said so per
                    // record. A conservative bound: every session of that build is marked,
                    // and a rollback to a pre-field build reads this manifest and writes none
                    // of it back, so every tab it later hands across is marked too. The mark
                    // is never lowered (the hook is a no-op in such a tab) and leaves when the
                    // tab closes.
                    frozen_path: crate::session_store::predates_path_self_heal(
                        manifest.outgoing_build,
                    ) || rec.frozen_path,
                    // Broadcast opt-ins: the topic set rides the record, validated onto the
                    // fabric (`fabric::parse_topics`).
                    topics: rec.topics.clone(),
                    // The foreground holder, off the record, so it arrives whatever rung the
                    // screen crossed at and whether or not the control carry did. A value that
                    // is no pgid is none.
                    fg_holder: rec.fg_holder.filter(|pgid| *pgid > 0).unwrap_or(0),
                    // The re-key channel: whether this shell reads a re-key file.
                    rekey: rec.rekey,
                    // The loader: whether this shell reads a body pointer.
                    loader: rec.loader,
                    // The history carry, imported after Commit.
                    history,
                    // THE STANDING HALTS (round four, item 3): the rows as the
                    // record carried them, one per origin, read FAIL-CLOSED at
                    // the seed (`spawn::seed_adopted_fabric`) — whatever rung
                    // the screen crossed at, and whether or not the proof
                    // covers it, because a halt is not the screen's to lose.
                    holds: rec.hold_rows(),
                    // The record's title, the fallback for a checkpoint that carried none
                    // (`spawn::seed_adopted_title`), unless the layout names the session with
                    // a title its park read, which replaces it below.
                    title: rec.title.clone(),
                    // Another supervisor's lease and the keyed escalations as the record
                    // carried them (read row by row at the seed, `seed_carried_claims`), and
                    // whether this build's host must hold off the session while an external
                    // supervisor claims it again.
                    supervisor: rec.supervisor.clone(),
                    attention_owners: rec.attention_owners.clone(),
                    claim_grace: rec.claim_grace(),
                    // The handoff gap: the build each adopted session's recorders name as
                    // where they start.
                    outgoing_build: manifest.outgoing_build,
                    // An update's handoff: the keeper offered nothing here.
                    keeper: None,
                    // The render debt the parent's engine owed the screen, judged by this
                    // build's epoch judge and repaint nudge.
                    render_debt,
                    // The mailbox and the timeline's count as the record carried them; the
                    // mailbox is re-validated row by row at the seed.
                    fabric: rec.fabric.clone(),
                    timeline_id: rec.timeline_id,
                    // THE SESSION LOG'S EPOCH, NODE-ID COUNTERS AND SEQ
                    // COUNT: a row that does not read carries none, never the
                    // manifest.
                    log: crate::session_log::LogAdopt {
                        ids: rec
                            .log_ids
                            .as_deref()
                            .and_then(crate::session_log::LogCarry::parse),
                        seq: rec.log_seq,
                        // The carried log rides the control sidecar, taken off
                        // it in `spawn_session` before the gap is marked.
                        lines: None,
                        keeper_ep: None,
                        mirror: None,
                    },
                    // The pair the crash journal keys the shell by, when the outgoing process
                    // held an image not its own; one that does not read is none.
                    journal_key: rec
                        .journal_key
                        .as_deref()
                        .and_then(crate::session_store::parse_journal_key),
                    // THE TASK RECORD (the worker pillar), as the record
                    // carried it, re-validated at the seed.
                    task: rec.task.clone(),
                    // THE AUTOPILOT'S END (§11.2.3, item 8): its face's time,
                    // seeded onto the timeline before registration.
                    autopilot_end: rec.autopilot_end.clone(),
                },
            })
        })
        .collect::<Option<Vec<_>>>();
    let Some(incoming) = incoming else {
        return refused_whole(
            WholeRefusal::Producer,
            "a session's identity, descriptor or screen sidecar was refused",
        );
    };
    // Commit to the carried bytes, for every adopted session, degraded or not. An exact
    // session's `checkpoint.grid`/`alt_grid` are the sidecar blobs verbatim
    // (`admit_incoming_screen` moves its input in unchanged); a degraded one keeps the bytes it
    // read beside its blank screen; `rec.screen.meta` is the parent's JSON unparsed. So this
    // hashes exactly what `write_outgoing` hashed, and never depends on this build's
    // `CheckpointMeta` serde shape matching the parent's.
    let Some(mut wire_entries) = incoming
        .iter()
        .map(|item| {
            let (grid, alt_grid) = match &item.degraded {
                Some((_, wire)) => (wire.grid.as_slice(), wire.alt_grid.as_deref()),
                None => {
                    let checkpoint = item.adopted.checkpoint.as_ref()?;
                    (checkpoint.grid.as_slice(), checkpoint.alt_grid.as_deref())
                }
            };
            let carry = manifest
                .sessions
                .iter()
                .find(|rec| rec.local_id == item.adopted.local_id)?
                .screen
                .as_ref()?;
            Some(ScreenWireEntry {
                local_id: item.adopted.local_id,
                meta: carry.meta.as_bytes(),
                grid,
                alt_grid,
            })
        })
        .collect::<Option<Vec<_>>>()
    else {
        return refused_whole(
            WholeRefusal::Own,
            "a session's carried screen could not be joined to its record",
        );
    };
    let Some(screen_digest) = screen_wire_digest(&mut wire_entries) else {
        return refused_whole(
            WholeRefusal::Producer,
            "the carried screens could not be framed for the adoption proof (too many \
             sessions, a duplicate id, or over the aggregate byte cap)",
        );
    };
    drop(wire_entries);
    // Say what did not cross, now that the adoption is certain: a handoff refused above never
    // logs a degrade that did not happen. A degraded screen's raw bytes were kept for the proof.
    if let Some(why) = unplaced_layout {
        aterm_log::warn!(
            "overlap handoff: the carried layout {why}; adopting all {} session(s) as tabs of \
             the first window instead of refusing the update",
            incoming.len()
        );
    }
    // The park's title, not the record's copy. A screen that carried no title (a producer
    // through v0.98.0, or a title that did not read) falls back to one the spawn installs as
    // the program's (`spawn::seed_adopted_title`). The record's copy is the store's, which the
    // outgoing build updates for a window's active tab only, so a background tab that retitled
    // since it was in front would hand over a stale title (`cargo test`, a frozen spinner
    // frame). The layout leaf naming the session carries the engine's `(cwd, title)` as the
    // park read them (`restore_session_meta`), covered by the layout digest; when the layout
    // names the session, its title is the fallback, empty included (which installs nothing).
    //
    // Except a read that missed, in front: that capture only tries the engine lock and writes
    // `(no cwd, "")` when it is busy (control-socket threads, the reflow and compression
    // workers), so a leaf with neither is a program that set none or a read that did not
    // happen. For the pane in front of its window (`RestoreManifest::front_terminal_ids`) the
    // record's copy is current (`publish_active_terminal_title`), so it stands. A pane behind
    // keeps its leaf's empty title, since its record copy is the stale one and its folder is
    // never wrong. With no placed layout the record's copy stands.
    let park_titles = layout
        .as_ref()
        .map(crate::restore::RestoreManifest::handed_off_leaves)
        .unwrap_or_default();
    let in_front = layout
        .as_ref()
        .map(crate::restore::RestoreManifest::front_terminal_ids)
        .unwrap_or_default();
    let adopted = incoming
        .into_iter()
        .map(|item| {
            match &item.degraded {
                Some((cause, _)) => aterm_log::warn!(
                    "overlap handoff: adopted session {} with a degraded screen: {cause}",
                    item.adopted.local_id
                ),
                None if item.adopted.repaint => aterm_log::info!(
                    "overlap handoff: adopted session {} onto the blank screen the outgoing \
                     build carried for a repaint",
                    item.adopted.local_id
                ),
                None => {}
            }
            let mut adopted = item.adopted;
            if !adopted
                .checkpoint
                .as_ref()
                .is_some_and(|cp| cp.title.is_some())
                && let Some(leaf) = park_titles
                    .iter()
                    .find(|leaf| leaf.local_id == Some(adopted.local_id))
                && !(leaf.cwd.is_none()
                    && leaf.title.is_empty()
                    && in_front.contains(&adopted.local_id))
            {
                adopted.title.clone_from(&leaf.title);
            }
            adopted
        })
        .collect::<Vec<_>>();
    // The held panes, last: every handed session was admitted first, so a held screen only
    // takes what they left of the aggregate caps, and nothing about one can refuse the handoff.
    let held = match shape {
        ReceiverShape::Current => take_held_panes(
            &manifest.held,
            &manifest
                .sessions
                .iter()
                .map(|rec| (rec.local_id, rec.sid.as_str()))
                .collect::<Vec<_>>(),
            manifest_path,
            manifest_stem,
            &dir,
            &mut remaining_grid_bytes,
            &mut used_grid_cells,
            &mut remaining_history_bytes,
            &mut remaining_control_bytes,
        ),
        #[cfg(all(test, unix))]
        ReceiverShape::PreCarry => Vec::new(),
    };
    // Turn ids go on above the outgoing process's count and every carried record, while this
    // process is still single-threaded, so a `since-turn=`/`since=` anchor taken before the
    // update still means "after that turn" and no new turn reuses a carried id.
    if let Some(minted) = manifest.next_turn_id {
        let floor = intake_turn_floor(minted);
        // A count no aterm mints (2^63 turns): the count goes on from the top of what a
        // manifest carries, and the outgoing process's reserve no longer fits above it, so ids
        // may repeat from here; logged.
        if floor >= crate::handoff_carry::MAX_TURN_ID {
            aterm_log::warn!(
                "update intake: the carried turn-id count ({minted}) is at the top of what a \
                 handoff carries; turn ids after this update may repeat ones the outgoing \
                 process used in its last moments"
            );
        }
        crate::control::raise_turn_ids(floor);
    }
    if let Some(resources) = manifest.resources.clone() {
        crate::resource_jobs::restore(resources);
    }
    // THIS MACHINE'S NAMES GO ON: the names the outgoing process read the kernel
    // give the machine, kept beside this process's own, so a carried shell's OSC 7
    // under the name it started with is still this machine's
    // (`tab_label::names_this_machine`).
    crate::tab_label::adopt_machine_names(&manifest.machine_names);
    for high in adopted
        .iter()
        .filter_map(|a| a.control.as_ref()?.high_turn_id())
        .chain(
            held.iter()
                .filter_map(|pane| pane.control.as_ref()?.high_turn_id()),
        )
    {
        crate::control::raise_turn_ids(high);
    }
    IncomingHandoff {
        adopted,
        window: manifest.window,
        nonce: Some(env_nonce),
        layout,
        layout_digest: Some(layout_digest),
        screen_digest: Some(screen_digest),
        connections: manifest.connections,
        unplaced_settings_drafts: unplaced_drafts,
        held,
        refusal: None,
        roster_seq: manifest.roster_seq,
        fabric_attached: manifest.fabric_attached,
        fleet_told: manifest.fleet_told,
        status_seen: manifest
            .sessions
            .iter()
            .filter_map(|rec| Some((rec.sid.clone(), rec.status_seen.clone()?)))
            .collect(),
        end_carry: end_carry_of(&manifest.sessions),
        #[cfg(unix)]
        person_stamps: Some(person_stamps_path(std::path::Path::new(&path))),
        mirror_dropped: manifest.mirror_dropped,
        mirror_served: manifest.mirror_served,
    }
}

/// THE CARRIED ENDS' INTAKE (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §11.2.3, item 8): each record's Autopilot end and its task's work
/// ([`crate::session_store::SessionRecord::autopilot_end`], read by
/// `harness_end_carry::EndCarry::parse`) by the sid it is adopted under —
/// the successor's `App::end_carry` (through
/// [`IncomingHandoff::end_carry`]), spent at each adopted session's first
/// publication (`App::adopt_end_carry`), never a reason to refuse. A record
/// with none, or one whose row does not read, gives none.
pub(crate) fn end_carry_of(
    sessions: &[crate::session_store::SessionRecord],
) -> std::collections::HashMap<String, crate::harness_end_carry::EndCarry> {
    sessions
        .iter()
        .filter_map(|rec| {
            let carry = crate::harness_end_carry::EndCarry::parse(rec.autopilot_end.as_deref()?)?;
            Some((rec.sid.clone(), carry))
        })
        .collect()
}

#[cfg(test)]
#[path = "seamless_tests.rs"]
pub(crate) mod tests;

#[cfg(all(test, unix))]
#[path = "seamless_carry_tests.rs"]
mod carry_tests;

#[cfg(test)]
#[path = "seamless_ladder_tests.rs"]
pub(crate) mod ladder_tests;

#[cfg(all(test, unix))]
#[path = "seamless_fixture_tests.rs"]
mod fixture_tests;

#[cfg(all(test, unix))]
#[path = "seamless_fixture_cover.rs"]
pub(crate) mod fixture_cover;

/// Tier-1 for `NativeUpdateSuccessorWarmBeforeClaim` (the handoff environment
/// snapshot, docs/DESIGN-warm-successor-2026-09-29.md P1).
#[cfg(all(test, unix))]
#[path = "seamless_handoff_env_tests.rs"]
pub(crate) mod handoff_env_conformance;

#[cfg(all(test, unix))]
#[path = "seamless_app_capture_tests.rs"]
mod app_capture_tests;

#[cfg(test)]
#[path = "seamless_f4_adoption_proof_asymmetry_tests.rs"]
mod f4_adoption_proof_asymmetry;
