// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Proof-carrying DSU Rung 1b — the LIVE seamless-update handoff wiring.
//!
//! The outgoing process keeps every original PTY master `FD_CLOEXEC`, creates a
//! child-owned `F_DUPFD_CLOEXEC` duplicate, and clears CLOEXEC only on the fork
//! child's copy in `pre_exec` so it survives that one `execve`. The survival fact is
//! proven with real `openpty`+`fork`+`execv` syscalls in `aterm-pty`. [`write_outgoing`] stamps
//! a nonce-authenticated [`SessionHandoff`] manifest into the per-user `0700` control dir
//! and returns the env the re-exec carries. The INCOMING process (the new binary, at the
//! top of `main` while still single-threaded) calls [`take_incoming`], which
//! authenticates + consumes the manifest, joins it to the volatile `(fd, pid)` channel
//! ([`HandoffFds`], env `ATERM_SEAMLESS_FDS`), and yields the [`Adopted`] set — each a live
//! shell to RE-ATTACH an engine + reader to (via `spawn_session(.., Some(adopted))`)
//! instead of forking a fresh one. So the running shell keeps going across the update.
//!
//! Fail-CLOSED throughout: a missing/mismatched/wrong-dir/replayed manifest, an
//! unparseable fd entry, or a non-tty fd all fall back to a normal fresh spawn — a
//! spoofed `ATERM_SEAMLESS_*` can only lose an adoption, never fabricate a session or
//! adopt a stranger's fd. The three env vars are CLEARED on read so they never leak into
//! the user's shell children.

use crate::session_store::{HandoffFds, ScreenCarry, SessionHandoff, WindowCarry};
use crate::spawn::Adopted;
use aterm_core::terminal::{CheckpointMeta, TerminalCheckpoint};
use aterm_session::{LaunchNonce, SessionId};
use sha2::{Digest, Sha256};

const ENV_MANIFEST: &str = "ATERM_SEAMLESS_MANIFEST";
pub(crate) const ENV_FDS: &str = "ATERM_SEAMLESS_FDS";
const ENV_NONCE: &str = "ATERM_SEAMLESS_NONCE";
const ENV_LAYOUT: &str = "ATERM_SEAMLESS_LAYOUT";
const ENV_TARGET: &str = "ATERM_SEAMLESS_TARGET";

const ADOPTION_PROOF_DOMAIN: &[u8] = b"aterm-seamless-adoption-v1\0";
const LAYOUT_PROOF_DOMAIN: &[u8] = b"aterm-seamless-layout-v1\0";
const SCREEN_PROOF_DOMAIN: &[u8] = b"aterm-seamless-screen-v1\0";
const MAX_HANDOFF_SESSIONS: usize = 256;
const MAX_HANDOFF_FDS_WIRE_BYTES: usize = 32 * 1024;
const MAX_HANDOFF_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_HANDOFF_LAYOUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_HANDOFF_GRID_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HANDOFF_AGGREGATE_GRID_BYTES: u64 = 256 * 1024 * 1024;
// The line codec can represent a very wide blank grid in a tiny wire. Bound the
// decoded allocation separately from encoded bytes, before `restore_grid`
// constructs any `Cell` vectors. Alt-screen checkpoints consume a second grid.
const MAX_HANDOFF_GRID_CELLS: u64 = 32 * 1024;
/// Maximum scrollback lines a handoff checkpoint may carry, per session.
///
/// The wire used to be defined as "exactly `rows` line records", which is why an
/// in-session update left every tab with a single screen of history. Carrying
/// history is therefore a WIRE change, and this constant is its ceiling: the
/// consumer bounds its allocation from the authenticated meta before decoding a
/// byte, so an untrusted `history_lines` above this is rejected outright rather
/// than believed.
///
/// 256 rather than the full 1000-line ring: `dimension_grid_cap` prices a line
/// generously (512 bytes per cell plus 16 KiB framing) precisely so a hostile
/// meta cannot authorize a huge decode, and that generosity multiplies against
/// every carried line. 256 takes a typical tab from ~50 lines of retained
/// history to ~300 — a real improvement — while keeping the per-grid ceiling in
/// the same order of magnitude it already had. The producer additionally
/// degrades to fewer lines, or none, under deadline pressure, so this is an upper
/// bound and never a requirement.
const MAX_HANDOFF_HISTORY_LINES: u32 = 256;

/// The producer's per-session history target. Same value as the wire ceiling —
/// the ceiling is what the CONSUMER will tolerate, this is what the PRODUCER
/// aims for, and keeping them equal means a healthy capture carries the maximum
/// the protocol allows.
#[must_use]
pub(crate) fn max_handoff_history_lines() -> u32 {
    MAX_HANDOFF_HISTORY_LINES
}

/// Ceiling on DECODED grid cells summed across every session in one handoff.
/// [`admit_checkpoint_dimensions`] charges it at all four seams — UI pre-capture,
/// outgoing digest, outgoing write, incoming decode — so splitting it per-seam
/// would let them disagree about what a pool costs.
///
/// This was 128 Ki cells, and that number was the bug. A `Cell` is 8 bytes
/// (compile-time asserted in `aterm-grid`), so the ceiling was ONE MEBIBYTE of
/// decoded cells for the whole application, sitting beside a 256 MiB encoded-byte
/// cap (`MAX_HANDOFF_AGGREGATE_GRID_BYTES`) — three orders of magnitude tighter
/// than the bound it was meant to complement. Each session costs
/// `cols * (2 * rows + history)` and the pool is every tab and pane of every
/// window, so an ordinary desk (a dozen panes at 49x110) exceeded it and the
/// in-session update deterministically refused to apply, forever, on that machine.
///
/// 4 Mi cells is 32 MiB of decoded cells: still an order of magnitude inside the
/// encoded caps that bound the wire independently, while `MAX_HANDOFF_GRID_CELLS`
/// still refuses a single absurd geometry and `MAX_HANDOFF_SESSIONS` still bounds
/// cardinality. Raising it is only half the fix — the other half lives in the
/// producer (`app_update_handoff`), which now reserves every session's mandatory
/// visible+alt cells before any session may spend the aggregate on optional
/// scrollback, so a later session can never find the budget already gone.
const MAX_HANDOFF_AGGREGATE_GRID_CELLS: u64 = 4 * 1024 * 1024;
const READY_WIRE_MAGIC: &[u8; 4] = b"ASR1";
const COMMIT_WIRE_MAGIC: &[u8; 4] = b"ASC1";
pub(crate) const READY_WIRE_LEN: usize = 4 + 4 + 32;

/// Whether an env set is one of the TWO legal handoff shapes.
///
/// This used to answer with a three-variant `HandoffProtocolShape` whose other
/// two variants described the v0.52/v0.53 bridge. When that bridge was deleted
/// from `take_incoming`, leaving the variants here made the two functions
/// DISAGREE: a legacy-shaped env set was still judged recognizable authority
/// (so its descriptors were preserved for the exec image) and was then refused
/// on arrival. One notion of "a legal handoff", in one place, is the point.
///
/// There are now two, and they are DISJOINT by construction:
///
/// * INHERITED (the fork lane): manifest + nonce + layout, plus the three
///   descriptor channels named by fd NUMBER — `fds`, `ready`, `commit`.
/// * OUT OF BAND (the LaunchServices lane): manifest + nonce + layout, plus a
///   rendezvous socket and a claim secret, and NONE of the fd-number channels.
///
/// The negative clauses are load-bearing rather than tidy. A LaunchServices
/// launch inherits no descriptors at all, so an fd NUMBER arriving in an
/// out-of-band environment cannot name what the outgoing process meant — it
/// names whatever happens to sit at that index in a table LaunchServices built.
/// Refusing the mixture is what stops a successor from adopting a stranger's
/// descriptor as a terminal master.
/// Which handoff env names are PRESENT. A named set rather than eight positional
/// booleans: the shape rule below reads every one of them, and at eight
/// same-typed parameters a transposed pair is both easy to write and invisible
/// at the call site — while the thing being described really is one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// One adopted session's identity in the handoff protocol: the pool-local
/// session id, the inherited PTY master fd, and the shell's pid, in that order.
/// This triple is the unit the fd channel carries, the unit
/// [`validated_identities`] proves is a bijection against the manifest, and the
/// unit [`adoption_proof`] hashes — naming it keeps those three agreeing on
/// field ORDER, which a bare tuple of three integers cannot enforce.
///
/// The middle field is a TRANSPORT COORDINATE, not an identity. `local_id` and
/// `pid` mean the same thing in any process; a descriptor number means "the slot
/// this PTY occupies in one process's descriptor table", and the two sides only
/// agree on it because `fork`+`execve` copies that table verbatim. See
/// [`adoption_proof`] for what that costs the proof and what replaces it once
/// the masters travel over `SCM_RIGHTS`.
pub(crate) type SessionIdentity = (u64, i32, i32);

/// Exact, attempt-bound proof of the PTYs the incoming process really adopted.
/// The nonce binds it to one outgoing manifest; the sorted identity triples bind
/// both cardinality and membership, so zero/subset/duplicate adoption cannot be
/// mistaken for complete readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdoptionProof {
    count: u32,
    digest: [u8; 32],
}

impl AdoptionProof {
    fn to_wire_with_magic(self, magic: &[u8; 4]) -> [u8; READY_WIRE_LEN] {
        let mut wire = [0u8; READY_WIRE_LEN];
        wire[..4].copy_from_slice(magic);
        wire[4..8].copy_from_slice(&self.count.to_be_bytes());
        wire[8..].copy_from_slice(&self.digest);
        wire
    }

    #[must_use]
    pub(crate) fn to_wire(self) -> [u8; READY_WIRE_LEN] {
        self.to_wire_with_magic(READY_WIRE_MAGIC)
    }

    #[must_use]
    pub(crate) fn to_commit_wire(self) -> [u8; READY_WIRE_LEN] {
        self.to_wire_with_magic(COMMIT_WIRE_MAGIC)
    }

    #[must_use]
    pub(crate) fn from_wire(wire: &[u8; READY_WIRE_LEN]) -> Option<Self> {
        Self::from_wire_with_magic(wire, READY_WIRE_MAGIC)
    }

    fn from_wire_with_magic(wire: &[u8; READY_WIRE_LEN], magic: &[u8; 4]) -> Option<Self> {
        if &wire[..4] != magic {
            return None;
        }
        let count = u32::from_be_bytes(wire[4..8].try_into().ok()?);
        let digest = wire[8..].try_into().ok()?;
        Some(Self { count, digest })
    }

    #[must_use]
    pub(crate) fn commit_wire_matches(self, wire: &[u8; READY_WIRE_LEN]) -> bool {
        Self::from_wire_with_magic(wire, COMMIT_WIRE_MAGIC) == Some(self)
    }
}

/// Canonical SHA-256 commitment to one complete adopted session set: the attempt
/// nonce, the authorized target build/commit, the layout and screen digests, and
/// the sorted [`SessionIdentity`] triples. Sorting makes it order-free; hashing
/// the count beside the members is what makes zero/subset/duplicate adoption
/// unmistakable rather than merely unlikely.
///
/// FROZEN CROSS-VERSION WIRE. The parent runs the OUTGOING build's copy of this
/// function, the child runs the INCOMING build's, and the two digests must be
/// equal — an update crosses a version boundary by definition. Nothing in the
/// protocol negotiates the format or even notices a disagreement: the parent
/// reports `AdoptionMismatch`, which `finish_update_handoff` classifies as a
/// genuine failure and latches manual-only, and every apply mode goes through
/// this same overlap. So changing ANY hashed byte retires the automatic update
/// lane for every parent already in the field. `f4_adoption_proof_asymmetry`
/// below is what happened the last time an input to this digest drifted between
/// two builds, and it is why the fd term survives the B4 section below.
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
/// Move the masters to `SCM_RIGHTS` (B4, `tests/handoff_launchd_job.rs`) and the
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
/// sketched in `tests/handoff_launchd_job.rs` is transport-portable but as
/// content-free as the number: it names a slot, so it detects a permutation and
/// never a substitution. It is fine as ADDRESSING — which received descriptor
/// is which `local_id` — and useless as proof.
///
/// The two halves of that paragraph a running kernel can settle are now settled
/// in the gate rather than by a note about what somebody once observed:
/// `the_replacement_proof_term_distinguishes_masters_and_survives_dup` opens
/// three real masters at once, asserts three DISTINCT minors, and asserts `dup`
/// preserves each — the substitution half and the same-open-file-description
/// half. The `SCM_RIGHTS` half stays an observation until there is fd-passing
/// code to assert it against; it follows from the same open file description,
/// but this file does not get to claim what it cannot run.
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
#[must_use]
pub(crate) fn adoption_proof(
    nonce: &str,
    target_build: u64,
    target_commit: &str,
    layout_digest: &[u8; 32],
    screen_digest: &[u8; 32],
    identities: &[SessionIdentity],
) -> Option<AdoptionProof> {
    let count = u32::try_from(identities.len()).ok()?;
    let mut identities = identities.to_vec();
    identities.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(ADOPTION_PROOF_DOMAIN);
    hasher.update(u32::try_from(nonce.len()).ok()?.to_be_bytes());
    hasher.update(nonce.as_bytes());
    hasher.update(target_build.to_be_bytes());
    let commit_prefix = normalize_commit(target_commit)?;
    hasher.update(u32::try_from(commit_prefix.len()).ok()?.to_be_bytes());
    hasher.update(commit_prefix.as_bytes());
    hasher.update(layout_digest);
    hasher.update(screen_digest);
    hasher.update(count.to_be_bytes());
    for (local_id, master, pid) in identities {
        hasher.update(local_id.to_be_bytes());
        hasher.update(master.to_be_bytes());
        hasher.update(pid.to_be_bytes());
    }
    Some(AdoptionProof {
        count,
        digest: hasher.finalize().into(),
    })
}

/// The ONE canonical spelling of a build's git commit, shared by every side of
/// the handoff. The parent's expectation comes from a release ticket (which may
/// carry a full 40-hex sha); the child's comes from its compiled-in
/// `build_info::GIT_COMMIT` (12 hex, optionally `-dirty`). Both must reduce to
/// the same string or the two sides are provably describing different binaries.
///
/// Extracted out of [`adoption_proof`] deliberately: the build/commit identity
/// is now an EXPLICIT equality check between the parent's expectation and the
/// child's self-report (see [`take_target_identity`]), not an implicit hash
/// input that silently poisons an otherwise-correct digest.
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
    // `unknown` is only ever a debug/dev shape; it must never authorize a
    // release handoff.
    (base == "unknown"
        && !dirty
        && (cfg!(debug_assertions) || std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some()))
    .then(|| "unknown".to_string())
}

/// Canonical, attempt-bound commitment to the complete window/tab/pane layout.
///
/// BYTE-IDENTITY LAW (the fix for the cross-version `AdoptionMismatch`): this is
/// a pure function of the *wire the parent writes*, and both sides MUST hash
/// those same bytes. The parent writes exactly `layout.to_toml()` to the layout
/// sidecar (`restore::write_to`) and hashes it here; the child hashes the bytes
/// it READ from that sidecar ([`layout_wire_digest`]) instead of re-serializing
/// its own parse. Re-serialization was the bug: it required the NEW binary's
/// TOML codec to be a byte fixed point on the OLD binary's wire, which no
/// schema bump, added `serde` field, or field reorder preserves — and an update
/// crosses a version boundary by definition.
#[must_use]
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
    Some(hasher.finalize().into())
}

/// Canonical commitment to every required visible-screen checkpoint. Sorting
/// by local id removes capture-order ambiguity; length framing prevents any
/// concatenation alias. Any semantic mutation to canonical meta/main/alt data
/// changes the adoption proof the parent expects; equivalent non-canonical wire
/// encodings are rejected before hashing.
#[must_use]
pub(crate) fn screen_digest(screens: &[(u64, TerminalCheckpoint)]) -> Option<[u8; 32]> {
    screen_digest_refs(
        screens
            .iter()
            .map(|(local_id, checkpoint)| (*local_id, checkpoint))
            .collect(),
    )
}

fn screen_digest_refs(mut screens: Vec<(u64, &TerminalCheckpoint)>) -> Option<[u8; 32]> {
    if screens.len() > MAX_HANDOFF_SESSIONS {
        return None;
    }
    screens.sort_unstable_by_key(|(local_id, _)| *local_id);
    if screens.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return None;
    }
    let mut aggregate_cells = 0_u64;
    // Project each live checkpoint into EXACTLY the bytes `write_outgoing` puts
    // on the wire (`serde_json` meta + the verbatim grid blobs), then hash those
    // bytes. The child hashes the same three byte strings after reading them
    // back, so the two digests agree by construction rather than by both codecs
    // happening to be fixed points across a version boundary.
    let mut metas = Vec::new();
    metas.try_reserve_exact(screens.len()).ok()?;
    for (local_id, checkpoint) in &screens {
        let meta = CheckpointMeta::from_checkpoint(checkpoint);
        let cap = checkpoint_grid_cap(&meta)?;
        if admit_checkpoint_dimensions(
            &mut aggregate_cells,
            checkpoint.rows,
            checkpoint.cols,
            checkpoint.history_lines,
            checkpoint.alt_grid.is_some(),
        )? != cap
        {
            return None;
        }
        if !checkpoint.parser_ground
            || checkpoint.alt_grid.is_some() != checkpoint.alt_cursor.is_some()
            || !checkpoint_grid_is_canonical(
                &checkpoint.grid,
                checkpoint.rows,
                checkpoint.cols,
                checkpoint.history_lines,
            )
            || u64::try_from(checkpoint.grid.len()).ok()? > cap
            || checkpoint.alt_grid.as_ref().is_some_and(|alt| {
                u64::try_from(alt.len()).is_ok_and(|len| len > cap)
                    // The alt screen keeps no scrollback: it is always exactly
                    // `rows` records, whatever the main grid carried.
                    || !checkpoint_grid_is_canonical(alt, checkpoint.rows, checkpoint.cols, 0)
            })
        {
            return None;
        }
        metas.push((*local_id, serde_json::to_vec(&meta).ok()?));
    }
    let mut entries = Vec::new();
    entries.try_reserve_exact(screens.len()).ok()?;
    for ((local_id, meta), (_, checkpoint)) in metas.iter().zip(screens.iter()) {
        entries.push(ScreenWireEntry {
            local_id: *local_id,
            meta,
            grid: &checkpoint.grid,
            alt_grid: checkpoint.alt_grid.as_deref(),
        });
    }
    screen_wire_digest(&mut entries)
}

/// One session's screen carry AS BYTES — precisely what crosses the wire.
struct ScreenWireEntry<'a> {
    local_id: u64,
    /// The `serde_json` `CheckpointMeta` blob (`ScreenCarry::meta`), verbatim.
    meta: &'a [u8],
    /// The main grid sidecar's `serialize_lines` bytes, verbatim.
    grid: &'a [u8],
    /// The alt grid sidecar's bytes when the carry declares one.
    alt_grid: Option<&'a [u8]>,
}

/// THE canonical screen commitment. Sorting by local id removes capture-order
/// ambiguity; length framing prevents any concatenation alias. Both the outgoing
/// process (over the bytes it is about to write) and the incoming process (over
/// the bytes it just read) call THIS function on THE SAME BYTES — that identity,
/// not codec luck, is what makes the adoption proof survive a version boundary.
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
    Some(hasher.finalize().into())
}

/// 16 CSPRNG bytes as 32 lowercase-hex chars, for the single-use handoff nonce — minted
/// via the workspace's ONE audited entropy surface ([`aterm_uds::rand::hex_token`]:
/// `getentropy(2)` with a BOUNDED `/dev/urandom` fallback, never a hand-rolled device
/// read). Entropy failure (never in practice) degrades to the all-zero token: guessable,
/// but a guessed nonce only ever COSTS the adoption — everything downstream fails closed —
/// it can never fabricate a session or adopt a stranger's fd, so fail-open is the right
/// posture here.
fn random_nonce() -> String {
    aterm_uds::rand::hex_token::<16>().unwrap_or_else(|_| "0".repeat(32))
}

/// Write `bytes` to `path` with owner-only permissions (the manifest's `0600`
/// posture); `None` on any failure.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Option<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut f| f.write_all(bytes))
            .ok()
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).ok()
    }
}

/// Read a regular file without following a symlink and without allocating past
/// its protocol cap. The caller supplies an exact private-dir path; it is
/// consumed on every parse outcome so an invalid attempt cannot be replayed.
fn take_regular_capped(
    path: &std::path::Path,
    dir: &std::path::Path,
    max_bytes: u64,
) -> Option<Vec<u8>> {
    if !path.starts_with(dir) {
        return None;
    }
    let result = (|| {
        use std::io::Read as _;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
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
    })();
    let _ = std::fs::remove_file(path);
    result
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

fn checkpoint_cursor_is_bounded(
    cursor: &aterm_core::terminal::GridCursorRepr,
    rows: u16,
    cols: u16,
) -> bool {
    cursor.cursor_row < rows
        && cursor.cursor_col < cols
        && cursor.scroll_top <= cursor.scroll_bottom
        && cursor.scroll_bottom < rows
        && cursor.margin_left <= cursor.margin_right
        && cursor.margin_right < cols
        // Grid resize deliberately never shrinks the stop vector: stops beyond
        // the current width must survive a later grow. Require coverage of every
        // active column, while independently capping the carried allocation at
        // the engine's protocol maximum.
        && cursor.tab_stops.len() >= usize::from(cols)
        && cursor.tab_stops.len() <= usize::from(aterm_core::grid::MAX_GRID_COLS)
}

fn checkpoint_meta_is_bounded(meta: &CheckpointMeta) -> bool {
    let rows = meta.rows;
    let cols = meta.cols;
    rows > 0
        && cols > 0
        && rows <= aterm_core::grid::MAX_GRID_ROWS
        && cols <= aterm_core::grid::MAX_GRID_COLS
        && checkpoint_cursor_is_bounded(&meta.cursor, rows, cols)
        && meta
            .alt_cursor
            .as_ref()
            .is_none_or(|cursor| checkpoint_cursor_is_bounded(cursor, rows, cols))
        && meta
            .saved_cursor_main
            .as_ref()
            .is_none_or(|cursor| cursor.cursor_row < rows && cursor.cursor_col < cols)
        && meta
            .saved_cursor_alt
            .as_ref()
            .is_none_or(|cursor| cursor.cursor_row < rows && cursor.cursor_col < cols)
        && meta
            .current_working_directory
            .as_ref()
            .is_none_or(|cwd| cwd.len() <= 8 * 1024 && !cwd.contains('\0'))
}

fn parse_checkpoint_meta(carry: &ScreenCarry) -> Option<CheckpointMeta> {
    {
        if carry.schema != ScreenCarry::SCHEMA {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(&carry.meta).ok()?;
        let object = value.as_object()?;
        const REQUIRED: &[&str] = &[
            "rows",
            "cols",
            "cursor",
            "alt_cursor",
            "saved_cursor_main",
            "saved_cursor_alt",
            "modes",
            "style_fg_bits",
            "style_bg_bits",
            "style_flag_bits",
            "style_protected",
            "charset",
            "kitty_keyboard",
            "xterm_keyboard",
            "taskbar_progress",
            "secure_keyboard_entry",
            "current_working_directory",
        ];
        if !REQUIRED.iter().all(|key| object.contains_key(*key)) {
            return None;
        }
        serde_json::from_value(value).ok()
    }
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
    // Visible-only checkpoints contain exactly `rows` line-codec records. The
    // cap scales with cells plus bounded per-line framing/attribute overhead,
    // then has a protocol-wide ceiling so a maximum-dimension hostile meta can
    // never authorize a multi-gigabyte startup allocation.
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

fn checkpoint_grid_cap(meta: &CheckpointMeta) -> Option<u64> {
    checkpoint_meta_is_bounded(meta).then_some(())?;
    dimension_grid_cap(meta.rows, meta.cols, meta.history_lines)
}

/// One dimension/allocation admission seam shared by UI pre-capture,
/// outgoing digest/write, and incoming decode. The aggregate changes only on
/// success, so an over-budget checkpoint cannot leave partial authority.
pub(crate) fn admit_checkpoint_dimensions(
    used_cells: &mut u64,
    rows: u16,
    cols: u16,
    history: u32,
    has_alt: bool,
) -> Option<u64> {
    let cap = dimension_grid_cap(rows, cols, history)?;
    // The VISIBLE grid keeps its original per-grid ceiling: that bound exists to
    // refuse an absurd screen geometry and has nothing to do with history.
    let cells = u64::from(rows).checked_mul(u64::from(cols))?;
    if cells > MAX_HANDOFF_GRID_CELLS {
        return None;
    }
    // Carried history is priced on top, bounded separately by
    // MAX_HANDOFF_HISTORY_LINES (already enforced in `dimension_grid_cap`).
    let history_cells = u64::from(history).checked_mul(u64::from(cols))?;
    // The alt grid never carries history (the live alt screen keeps no
    // scrollback), so it costs one visible grid, not one carried grid.
    let cost = cells
        .checked_add(history_cells)?
        .checked_add(if has_alt { cells } else { 0 })?;
    let next = used_cells.checked_add(cost)?;
    if next > MAX_HANDOFF_AGGREGATE_GRID_CELLS {
        return None;
    }
    *used_cells = next;
    Some(cap)
}

/// What one session's MANDATORY carry costs the aggregate: its visible grid plus
/// the alt grid the producer conservatively reserves — exactly what
/// `admit_checkpoint_dimensions(used, rows, cols, 0, true)` charges.
///
/// Named separately because the producer must be able to price a WHOLE POOL
/// before charging any of it. `admit_checkpoint_dimensions` cannot answer that:
/// it is transactional per session and knows nothing about the sessions still
/// queued behind the one it is admitting.
#[must_use]
pub(crate) fn mandatory_checkpoint_cells(rows: u16, cols: u16) -> u64 {
    u64::from(rows)
        .saturating_mul(u64::from(cols))
        .saturating_mul(2)
}

/// The same pricing for the producer's capture-BYTE budget: twice
/// `dimension_grid_cap`, which is exactly what the capture loop charges per
/// admitted session (twice, because the alt grid is reserved conservatively for
/// the same reason `has_alt` is `true` at the pre-capture admission). `None` for
/// a geometry `dimension_grid_cap` refuses outright.
#[must_use]
pub(crate) fn checkpoint_capture_budget_bytes(rows: u16, cols: u16, history: u32) -> Option<u64> {
    dimension_grid_cap(rows, cols, history)?.checked_mul(2)
}

/// The process-wide aggregate cell ceiling, so the producer can price a whole
/// pool against the same number `admit_checkpoint_dimensions` enforces. See
/// `MAX_HANDOFF_AGGREGATE_GRID_CELLS` for why one shared constant, never a
/// per-seam copy.
#[must_use]
pub(crate) fn max_handoff_aggregate_grid_cells() -> u64 {
    MAX_HANDOFF_AGGREGATE_GRID_CELLS
}

fn checkpoint_grid_is_canonical(bytes: &[u8], rows: u16, cols: u16, history: u32) -> bool {
    // A materialized grid cell holds at most one 256-byte grapheme unit. Bound
    // content and full record framing from the authenticated column count before
    // the decoder allocates any line payload or sidecars.
    let content_cap = usize::from(cols).saturating_mul(256);
    let record_cap = 16usize
        .saturating_mul(1024)
        .saturating_add(usize::from(cols).saturating_mul(512));
    // A carried checkpoint is `history` scrollback records followed by exactly
    // `rows` visible records. `history` is bounded by the caller's meta check, so
    // this total can never be inflated by the payload itself.
    if history > MAX_HANDOFF_HISTORY_LINES {
        return false;
    }
    let Some(expected) = usize::from(rows).checked_add(history as usize) else {
        return false;
    };
    let Some(lines) = aterm_core::scrollback::deserialize_lines_strict(
        bytes,
        expected,
        usize::from(cols),
        content_cap,
        record_cap,
    ) else {
        return false;
    };
    lines.len() == expected && aterm_core::scrollback::serialize_lines(&lines).as_slice() == bytes
}

fn normalize_incoming_checkpoint_grid(
    bytes: Vec<u8>,
    rows: u16,
    cols: u16,
    history: u32,
) -> Option<Vec<u8>> {
    checkpoint_grid_is_canonical(&bytes, rows, cols, history).then_some(bytes)
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
    if nonce.len() != 32 || !nonce.as_bytes().iter().all(u8::is_ascii_hexdigit) {
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

/// The physical artifacts one outgoing attempt published, plus the ONE screen
/// commitment computed over the exact bytes it wrote.
pub(crate) struct OutgoingHandoff {
    pub manifest_path: String,
    pub nonce: String,
    pub fds_wire: String,
    /// [`screen_wire_digest`] over the carried meta/grid bytes as written. The
    /// worker asserts this equals the digest the main thread committed to before
    /// parking; a difference is a preparation failure, never a silent mismatch
    /// that only surfaces as the child's unexplained `AdoptionMismatch`.
    pub screen_digest: [u8; 32],
}

/// Write the outgoing handoff manifest (nonce-stamped, `0600`, in the `0700` control dir)
/// and return the [`OutgoingHandoff`] whose first three fields become the child's env.
/// `None` when there is no private control dir (then the caller re-execs WITHOUT the
/// seamless env → a normal cold apply). Every fd in `fds` is a child-owned CLOEXEC
/// duplicate; the caller clears the flag only in that command's `pre_exec` closure.
///
/// `screens` carries each session's engine checkpoint (by `local_id`): the scalar
/// meta is embedded in the matching manifest record, the grid byte blobs are
/// written as nonce-stamped `0600` sidecar files beside the manifest. `window`
/// is the outgoing window's frame. Screen carry is REQUIRED for an overlap:
/// every authenticated PTY must have one valid meta+grid checkpoint (and an
/// alt-grid blob when its checkpoint names one). Any omission fails the whole
/// preparation so a child can never Commit a blank or incomplete engine.
pub(crate) fn write_outgoing(
    manifest: &SessionHandoff,
    fds: &HandoffFds,
    screens: &[(u64, TerminalCheckpoint)],
    window: Option<WindowCarry>,
) -> Option<OutgoingHandoff> {
    let dir = crate::control_auth::socket_dir()?;
    let nonce = random_nonce();
    let path = dir.join(format!("seamless-{}-{nonce}.toml", std::process::id()));

    let _ = validated_identities(manifest, fds)?;
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
        return None;
    }

    // Attach the screen carries all-or-nothing. Track each blob so a partial
    // physical write is retired before returning PreparationFailed.
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
            return None;
        };
        let checkpoint_meta = CheckpointMeta::from_checkpoint(cp);
        let admitted_cap = admit_checkpoint_dimensions(
            &mut aggregate_cells,
            cp.rows,
            cp.cols,
            cp.history_lines,
            cp.alt_grid.is_some(),
        );
        if !cp.parser_ground
            || admitted_cap.is_none()
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
            return None;
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
            return None;
        }
        let Ok(meta) = serde_json::to_string(&checkpoint_meta) else {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return None;
        };
        let grid_file = dir.join(format!(
            "seamless-{}-{nonce}.s{local_id}.grid",
            std::process::id()
        ));
        if write_private(&grid_file, &cp.grid).is_none() {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return None;
        }
        written_blobs.push(grid_file.clone());
        let alt_grid_file = if let Some(alt) = cp.alt_grid.as_ref() {
            let p = dir.join(format!(
                "seamless-{}-{nonce}.s{local_id}.altgrid",
                std::process::id()
            ));
            if write_private(&p, alt).is_none() {
                for path in written_blobs {
                    let _ = std::fs::remove_file(path);
                }
                return None;
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
        });
    }
    // THE commitment, taken over the bytes actually published above (the same
    // meta strings, the same grid blobs). The child recomputes it from the bytes
    // it reads back, so both sides hash one byte string with one function.
    let mut wire_entries = Vec::with_capacity(screens.len());
    for (local_id, cp) in screens {
        let Some((_, meta)) = carried_metas.iter().find(|(id, _)| id == local_id) else {
            for path in written_blobs {
                let _ = std::fs::remove_file(path);
            }
            return None;
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
        return None;
    };
    let manifest = &manifest;

    let Some(toml) = manifest.to_toml().ok() else {
        for path in written_blobs {
            let _ = std::fs::remove_file(path);
        }
        return None;
    };
    // First line is the nonce (bound to the env copy); the rest is the manifest TOML.
    let body = format!("{nonce}\n{toml}");
    // 0600: readable only by us (the dir is already 0700 per-user).
    #[cfg(unix)]
    let write_ok = {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| f.write_all(body.as_bytes()))
            .is_ok()
    };
    #[cfg(not(unix))]
    let write_ok = std::fs::write(&path, &body).is_ok();
    if !write_ok {
        for path in written_blobs {
            let _ = std::fs::remove_file(path);
        }
        return None;
    }
    Some(OutgoingHandoff {
        manifest_path: path.to_string_lossy().into_owned(),
        nonce,
        fds_wire: fds.encode(),
        screen_digest: carry_digest,
    })
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
    /// Exact manifest/fd identity set claimed by the authenticated outgoing
    /// process. Retained for the one-release legacy one-channel bridge: the new
    /// child signals an old parent only when its ACTUAL adopted pool equals this.
    /// Attempt-bound layout sidecar consumed from the same private nonce prefix.
    pub layout: Option<crate::restore::RestoreManifest>,
    /// [`layout_wire_digest`] over the EXACT sidecar bytes this process read —
    /// never over a re-serialization of `layout`. The outgoing process hashed
    /// the identical bytes when it wrote them, so this matches across a version
    /// boundary; re-serializing here is what produced `AdoptionMismatch`.
    /// `None` on the legacy bridge, which carries no attempt-bound sidecar and
    /// never computes an adoption proof.
    pub layout_digest: Option<[u8; 32]>,
    /// [`screen_wire_digest`] over the EXACT carried meta/grid bytes, for the
    /// same reason.
    pub screen_digest: Option<[u8; 32]>,
}

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

    fn transfer_all(&mut self) {
        self.0.clear();
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

/// OVERLAP-HANDOFF rollback: delete every artifact a [`write_outgoing`] with
/// this `nonce` produced (the manifest + its screen sidecar blobs — all named
/// `seamless-<pid>-<nonce>…` in our 0700 control dir). Under exec the child
/// consumed them or a failed exec left them for the stale sweep; under the
/// overlap a dead PRE-CONSUME child leaves them live on disk, and the parent —
/// which is still running — must retire them itself (a fresh nonce is minted
/// per retry, so nothing here blocks a later attempt). Best-effort, prefix-
/// bound to our own pid + the exact nonce: never unlinks anything we did not
/// write this attempt.
pub(crate) fn discard_outgoing(nonce: &str) {
    let Some(dir) = crate::control_auth::socket_dir() else {
        return;
    };
    let prefix = format!("seamless-{}-{nonce}", std::process::id());
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The overlap-handoff readiness pipe's write fd, passed by the PARKED parent
/// (`apply_staged_update_now`) so this incoming process can tell it "every
/// carried window is painted — exit under me". Its own env var (never the
/// `ATERM_SEAMLESS_FDS` wire: it is not a session, and it must FAIL the
/// `fd_is_tty` backstop by design).
pub(crate) const ENV_READY_FD: &str = "ATERM_HANDOFF_READY_FD";
pub(crate) const ENV_COMMIT_FD: &str = "ATERM_HANDOFF_COMMIT_FD";
/// OUT-OF-BAND LANE: the single-use rendezvous socket the outgoing process bound
/// BEFORE it launched this successor. Its PRESENCE is the lane — there is no
/// version byte anywhere, because a parent can only publish this name if it has
/// out-of-band transport code, which is what makes "no parent already in the
/// field can ever be shown the new shape" structural rather than argued.
pub(crate) const ENV_RENDEZVOUS: &str = "ATERM_HANDOFF_RENDEZVOUS";
/// The secret that admits exactly one dialer to that rendezvous.
pub(crate) const ENV_CLAIM: &str = "ATERM_HANDOFF_CLAIM";
const ENV_PARENT_PID: &str = "ATERM_HANDOFF_PARENT_PID";
/// The outgoing process's KERNEL BIRTH RECORD, published beside its pid. A pid
/// alone is a recyclable number; this is what makes it an identity the
/// successor can verify without being the outgoing process's fork child. See
/// [`AttestedParent`] for why that distinction is the whole point.
const ENV_PARENT_BIRTH: &str = "ATERM_HANDOFF_PARENT_BIRTH";

/// The kernel's own birth record for a process: the microsecond-resolution
/// instant `fork` created it. Assigned by the kernel, so no process can choose
/// its own; and two processes that reuse a pid cannot share it, which is
/// exactly the property a bare pid lacks. Compared for EQUALITY only — it is an
/// identity token, never a clock reading.
///
/// The same idea already guards the managed-Ollama daemon
/// (`title_summary::managed_ollama::ManagedProcessIdentity`, "a bare PID is not
/// an identity: it can be reused after exit"); this is the update lane's copy,
/// kept local because the handoff needs the probe on its own hot path and must
/// not gain a dependency on the summary subsystem.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessBirth {
    seconds: u64,
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

/// Read the kernel's record for `pid`, or `None` when there is no process we
/// may treat as a handoff parent there.
///
/// `None` covers four distinct facts on purpose, because the caller's response
/// to all four is identical — refuse:
///
/// * no such process (the parent already exited AND was reaped);
/// * a ZOMBIE. Treating it as dead keeps this probe's edge NEAR the predicate
///   it replaces, not exactly on it: XNU's `proc_exit()` reparents children to
///   initproc BEFORE setting `p_stat = SZOMB`, so `getppid()` flips first and
///   this probe goes false strictly later — same event, a few instructions
///   apart. The gap is on the safe side (the successor keeps running slightly
///   longer than the old predicate would have allowed, and the commit pipe
///   still bounds it); claiming the two edges coincide would be wrong. libproc
///   may already refuse zombies outright, in which case this arm never runs;
/// * a process owned by another uid. Every other authority in the overlap
///   protocol is uid-bounded (the 0700 control dir, `control_auth::peer_check`),
///   and a cross-uid "parent" is not a shape this lane has;
/// * the kernel disagreeing about the pid it just reported on.
#[cfg(target_os = "macos")]
fn read_process_birth(pid: libc::pid_t) -> Option<ProcessBirth> {
    if pid <= 1 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    // SAFETY: `geteuid` is a side-effect-free libc getter.
    let ours = unsafe { libc::geteuid() };
    if u32::try_from(pid).ok()? != info.pbi_pid || info.pbi_uid != ours {
        return None;
    }
    if info.pbi_status == libc::SZOMB {
        return None;
    }
    Some(ProcessBirth {
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

/// Other unixes have no birth-record primitive wired here, so they can only
/// ever attest through the kernel parent link — which is sound there because
/// they have no LaunchServices lane: off macOS the successor is unconditionally
/// a fork child of the outgoing process (`Command::spawn` in
/// `app_update_handoff::run_handoff_worker` is the only launch shape), so the
/// parent link always exists. If a non-fork transport is ever added off macOS,
/// this function must gain a real implementation FIRST — `/proc/<pid>/stat`
/// field 22 is Linux's equivalent — because [`attest_handoff_parent`] then has
/// no witness left to offer.
#[cfg(all(unix, not(target_os = "macos")))]
fn read_process_birth(_pid: libc::pid_t) -> Option<ProcessBirth> {
    None
}

/// This process's own birth record, for publication to a successor.
#[cfg(unix)]
#[must_use]
fn own_process_birth() -> Option<ProcessBirth> {
    read_process_birth(libc::pid_t::try_from(std::process::id()).ok()?)
}

/// How the successor learned WHICH process its handoff parent is. The two arms
/// are not interchangeable implementations of one probe: they are the two
/// mutually exclusive situations a successor can be born into, each carrying
/// the strongest identity primitive available in it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentWitness {
    /// The kernel's birth record for the attested pid, verified at admission.
    /// Independent of the process tree, so it keeps answering for a successor
    /// launchd owns — this is the arm B1 needs.
    Birth(ProcessBirth),
    /// The kernel parent link. Carries no information once `getppid()` is 1, so
    /// it is only ever chosen while the attested pid IS our live creator. Used
    /// when libproc declined to produce a birth record, so that a machine where
    /// the update lane's liveness probe is unavailable still updates.
    ForkLink,
}

/// PARENTAL ATTESTATION — the ONE witness the overlap protocol trusts about who
/// the outgoing process is and whether it is still alive. Both halves are
/// load-bearing, and they fail in opposite directions:
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
/// 2. LIVENESS — re-evaluated continuously, for the whole overlap. Its edge must
///    be the parent's `exit`, and it must be immune to PID REUSE. This is the
///    dangerous direction: a false "alive" does not lose a handoff, it strands a
///    readerless candidate holding duplicated masters of the user's live shells
///    that can never be committed — see the `_exit(74)` in
///    [`CommitReceiver::start_watch`], for which this is the backstop underneath
///    pipe EOF (EOF cannot fire if a write end leaked, which Darwin's
///    non-atomic pipe+`FD_CLOEXEC` sequence permits).
///
/// `getppid() == pid` used to provide both at once, which is why it read as a
/// single cheap check: the parent link is kernel state no environment can
/// forge (1), it flips inside the parent's `exit(2)` — at exit, NOT at reap, so
/// strictly before the pid can be recycled — and on macOS its only replacement
/// value is 1, permanently (2). But it provides both ONLY to a fork child.
///
/// BLOCKER B1 (see the KNOWN DEFECT note at the `spawn` in
/// `app_update_handoff::run_handoff_worker`): a successor launched through
/// LaunchServices has ppid 1 from birth, so `getppid()` cannot name its parent
/// and the old predicate refused it at admission and then fail-stopped it.
/// The repair is to stop conflating the two properties:
///
/// * LIVENESS moves to the birth record, which is transport-independent. It
///   discriminates by a kernel-assigned microsecond stamp rather than by a pid,
///   so a recycled pid fails it, and it treats a zombie as dead, so its edge is
///   still the parent's `exit`.
///
///   ONE PLACE IT IS GENUINELY WEAKER, stated because the alternative is a
///   comment that lies about a security property: the LEGACY-CAPTURE arm
///   (`creator == pid`, i.e. today's fork transport) reads `getppid()` and then
///   `read_process_birth(pid)` as TWO calls, where the predicate it replaces
///   was a single kernel-atomic comparison. That is a TOCTOU window. Exploiting
///   it needs the parent to exit, be reaped, and the pid space to wrap ~100k
///   sequential allocations onto that exact number, all between two adjacent
///   syscalls — and the commit-pipe EOF backstops it independently. Small, but
///   not zero, and it is the same race the earlier attempt cited when it
///   refused to touch B1 at all.
/// * IDENTITY stays with the strongest primitive the successor's actual birth
///   situation offers. With a live creator that is still the parent link, and
///   the published claim must AGREE with it — unchanged, and unchanged
///   deliberately: this is the arm that refuses the leaked-env case. With no
///   live creator (`getppid() == 1`) the link says nothing at all, and the
///   witness is the birth record the outgoing process published for itself,
///   which a stale or accidental value cannot match unless it names a process
///   that is genuinely still alive at that exact pid and birth instant.
///
/// The remaining gap is honest and bounded: against a SAME-UID adversary the
/// published birth record is copyable (it is public via `ps`), so the no-live-
/// creator arm rests on the uid boundary that the rest of this protocol already
/// rests on — the 0700 control dir the manifest must live in, and
/// `control_auth::peer_check`. B4's `SCM_RIGHTS` transport closes even that: a
/// socket's kernel-attested peer pid (`getsockopt(SOL_LOCAL, LOCAL_PEERPID)`,
/// recorded by the kernel at `connect(2)`) is unforgeable identity, and pairing
/// it with the birth record already implemented here also closes the
/// registration race that a peer pid alone would leave — a peer that died and
/// had its pid recycled before the receive fails the birth comparison. So this
/// function's shape is what B4 extends, not something B4 replaces.
///
/// `creator` is the kernel parent link, taken by the caller so the decision is
/// testable at both of its arms without needing a ppid-1 process.
#[cfg(unix)]
#[must_use]
fn attest_handoff_parent_from(
    pid: libc::pid_t,
    published: Option<ProcessBirth>,
    creator: libc::pid_t,
) -> Option<AttestedParent> {
    // pid 1 is never a real handoff parent: it is what reparenting produces.
    if pid <= 1 {
        return None;
    }
    if creator > 1 && creator != pid {
        // We HAVE a live creator and it is not who the environment named. No
        // amount of published detail may override the kernel here: this is the
        // arm that refuses a stale handoff block re-entering through a shell.
        return None;
    }
    let live = read_process_birth(pid);
    let witness = match (published, live) {
        // The outgoing process named its own birth instant and the kernel
        // agrees. Sound whether or not we are its child, which is all of B1.
        (Some(published), Some(live)) if published == live => ParentWitness::Birth(live),
        // A claim the kernel does not corroborate is corrupt authority, not a
        // reason to fall back to something weaker.
        (Some(_), _) => return None,
        // Legacy outgoing build: it published no birth record, but the parent
        // link proves this pid is our creator, so the record we read now is
        // provably the parent's own. Capturing it here is what lets the watch
        // below stop consulting the process tree.
        (None, Some(live)) if creator == pid => ParentWitness::Birth(live),
        (None, None) if creator == pid => {
            // On macOS a live same-uid process with no readable birth record is
            // a real anomaly (libproc answers for other processes here — the
            // managed-Ollama attestation depends on it), so say so: it is the
            // only signal that this machine's handoffs are running on the
            // weaker witness. Off macOS the parent link IS the documented
            // witness, so there is nothing to report.
            #[cfg(target_os = "macos")]
            eprintln!(
                "aterm-gui: seamless handoff: no kernel birth record for parent {pid}; \
                 falling back to the process link for parent-death detection"
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
    attest_handoff_parent_from(pid, published, unsafe { libc::getppid() })
}

/// A handoff parent whose identity was proven at admission. Its fields are
/// private to this module and [`attest_handoff_parent_from`] is the only
/// producer, so outside the attestation code above, HOLDING one is the proof —
/// the only question a holder can still ask is whether that process is alive.
/// This is why the value, not a bare pid, is what travels from
/// [`prearm_incoming_fds`] through [`take_commit_fd`] into [`CommitReceiver`]:
/// no later stage can accidentally re-derive a weaker claim.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AttestedParent {
    pid: libc::pid_t,
    witness: ParentWitness,
}

#[cfg(unix)]
impl AttestedParent {
    /// Is the attested process — that exact process, not merely something at
    /// its pid — still running?
    #[must_use]
    fn still_alive(self) -> bool {
        match self.witness {
            ParentWitness::Birth(birth) => read_process_birth(self.pid) == Some(birth),
            ParentWitness::ForkLink => {
                // SAFETY: `getppid` is a side-effect-free libc getter.
                let creator = unsafe { libc::getppid() };
                creator == self.pid
            }
        }
    }

    /// The authority pairs to restore onto the boot-apply re-exec image.
    ///
    /// A `Birth` witness republishes the record, so the re-exec'd image can
    /// re-attest without a parent link even when the ORIGINAL outgoing process
    /// published none — we only ever hold a `Birth` witness we proved, so
    /// publishing it forward is passing on a proof, not inventing one.
    fn exec_env(self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        let mut pairs = vec![(
            std::ffi::OsString::from(ENV_PARENT_PID),
            std::ffi::OsString::from(self.pid.to_string()),
        )];
        if let ParentWitness::Birth(birth) = self.witness {
            pairs.push((
                std::ffi::OsString::from(ENV_PARENT_BIRTH),
                std::ffi::OsString::from(birth.to_wire()),
            ));
        }
        pairs
    }
}

/// The parental-authority environment an OUTGOING process publishes to its
/// successor. Encoding lives here, next to the decoding in
/// [`prearm_incoming_fds`], so the two cannot drift.
///
/// The birth record is omitted when the kernel will not give us our own. That
/// omission is what keeps the two sides symmetric on a machine where libproc is
/// unavailable: the successor refuses a published record it cannot corroborate,
/// so publishing one we could not read ourselves would turn an unavailable
/// probe into a failed update. Omitting it instead costs a legacy-shaped
/// handoff — the successor attests through the parent link, exactly as every
/// build before 0.13 did — and never a broken one.
#[cfg(unix)]
#[must_use]
pub(crate) fn outgoing_parent_env() -> Vec<(&'static str, String)> {
    let mut pairs = vec![(ENV_PARENT_PID, std::process::id().to_string())];
    if let Some(birth) = own_process_birth() {
        pairs.push((ENV_PARENT_BIRTH, birth.to_wire()));
    }
    pairs
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

    /// Environment restored ONLY onto the updater's final same-process exec
    /// image, never the ambient process env — [`prearm_incoming_fds`] cleared
    /// the raw variable precisely so no codesign/PlistBuddy/spctl helper can
    /// ever observe it. The re-exec'd NEW binary re-runs prearm, whose
    /// authority predicate requires an `OverlapModern` handoff to carry the
    /// parental identity validated here; without this pair the successor
    /// classifies the inherited handoff malformed and exits before writing
    /// the readiness proof, so the waiting parent reads EOF (`ChildDied`)
    /// and the seamless lane can never succeed.
    pub(crate) fn final_exec_env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        if !self.valid {
            return Vec::new();
        }
        self.parent
            .map(AttestedParent::exec_env)
            .unwrap_or_default()
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

    /// The attested handoff parent. Named for the pid because that is what
    /// identifies it to a reader; what it actually carries is the proof, so
    /// that no later stage has to re-derive one from an environment that has
    /// since been scrubbed.
    pub(crate) fn parent_pid(&self) -> Option<AttestedParent> {
        self.valid.then_some(self.parent).flatten()
    }
}

#[cfg(unix)]
fn clear_handoff_env() {
    // Called only during the single-threaded startup seam (or under the
    // module-wide environment lock in tests), before any worker or user process
    // can concurrently observe environment mutation — and routed through the
    // workspace's one lock-scoped env helper rather than raw `remove_var`s.
    for key in [
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
    ] {
        aterm_log::env::unset(key);
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

/// The successor's entry-point posture repair — `main_entry` calls it before any
/// helper, session, window, or user code: re-arm CLOEXEC on every descriptor an
/// incoming handoff names, attest the outgoing parent, and scrub the authority
/// environment.
///
/// B3 — PROCESS-GROUP CONTAINMENT, RESIDUAL GAP (see
/// `tests/handoff_launchd_job.rs`). Today the parent puts the candidate in its
/// own process group with `setpgid(0, 0)` in `run_handoff_worker`'s `pre_exec`,
/// which is what makes that file's `kill(-pid, SIGKILL)` sweep the candidate's
/// `ditto`/`codesign`/`spctl` helpers instead of only its leader. A
/// LaunchServices launch has no pre-exec hook, so the call has to move HERE:
/// this function already runs before the successor can spawn anything, so every
/// helper would still inherit the group. That disposes of the stated
/// "helpers spawned before the call escape it" race, and of the
/// EPERM worry too — `setpgid(0, 0)` can only fail EPERM because the caller is a
/// session leader, and a session leader's group id already equals its pid, so
/// `getpgrp() == getpid()` holds either way and is the postcondition to assert
/// rather than the call's return value.
///
/// What does NOT survive the move, and cannot be repaired in this file:
///
/// * THE ORDERING GUARANTEE. `pre_exec` establishes the group BEFORE the
///   candidate's image runs, so `kill(-pid)` is a valid handle from the instant
///   `spawn` returns. Successor-side, there is a window between the launch and
///   this instruction in which the successor sits in whatever group the launcher
///   left it, and a rejection issued then signals a group the candidate is not
///   in. Process-group ids also share the pid number space, so if the
///   successor's pid was recycled from a reaped group leader whose group still
///   has members, `kill(-pid)` reaches strangers — the same pid-reuse hazard
///   [`AttestedParent`] closes for the parent's identity, except that a process
///   GROUP has no kernel birth record to attest.
/// * THE PARENT CANNOT LEARN THE GROUP EXISTS. The readiness wire is a fixed
///   proof record the parent computes for itself, so the successor has nowhere
///   to report its pgid. Until B4's control socket carries that attestation the
///   parent must treat `kill(pid)` as its only sound signal and `kill(-pid)` as
///   an unproven optimization — which is precisely the helper sweep B3 exists
///   for, and it also costs `emergency_kill_and_reap_handoff_child` its
///   "signal the group before any wait" ordering, whose whole point is that
///   reaping the leader releases the pid.
///
/// So B3 is achievable but not equivalent, and closing the difference is B2/B4
/// work (name the non-child successor, then carry its attested pgid), not a
/// local edit to this function.
#[cfg(unix)]
pub(crate) fn prearm_incoming_fds() -> PrearmedIncomingFds {
    let manifest_present = std::env::var_os(ENV_MANIFEST).is_some();
    let fds_present = std::env::var_os(ENV_FDS).is_some();
    let nonce_present = std::env::var_os(ENV_NONCE).is_some();
    let layout_present = std::env::var_os(ENV_LAYOUT).is_some();
    let ready_present = std::env::var_os(ENV_READY_FD).is_some();
    let commit_present = std::env::var_os(ENV_COMMIT_FD).is_some();
    // The out-of-band lane's two names. They are read here — beside every other
    // handoff name — so that ONE function decides what a recognizable handoff
    // environment is, and `clear_handoff_env` below cannot forget to wipe them.
    let rendezvous_present = std::env::var_os(ENV_RENDEZVOUS).is_some();
    let claim_present = std::env::var_os(ENV_CLAIM).is_some();
    let parent_present = std::env::var_os(ENV_PARENT_PID).is_some();
    let birth_present = std::env::var_os(ENV_PARENT_BIRTH).is_some();
    let published_birth = std::env::var(ENV_PARENT_BIRTH)
        .ok()
        .and_then(|value| ProcessBirth::from_wire(&value));
    let parent = std::env::var(ENV_PARENT_PID)
        .ok()
        .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
        .and_then(|pid| attest_handoff_parent(pid, published_birth));
    // Parent identity is now process-local typed state; never let the raw env
    // reach updater verification helpers or user shells.
    aterm_log::env::unset(ENV_PARENT_PID);
    aterm_log::env::unset(ENV_PARENT_BIRTH);
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
    if let Ok(wire) = std::env::var(ENV_FDS) {
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
        if let Some(wire) = std::env::var_os(ENV_FDS) {
            for entry in wire.as_os_str().as_bytes().split(|byte| *byte == b',') {
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
        let present = std::env::var_os(key).is_some();
        if let Ok(value) = std::env::var(key) {
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
    // A partial or legacy-shaped env set is now unrecognizable authority, so its
    // descriptors are closed here rather than carried to an exec image that would
    // refuse them anyway.
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
    // The birth record is OPTIONAL authority — a legacy outgoing build publishes
    // none — but an unparseable one is corruption, not absence, and must not
    // silently degrade admission to the weaker witness it exists to replace.
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
            clear_handoff_env();
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

    pub(crate) fn final_exec_env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        Vec::new()
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
pub(crate) fn prearm_incoming_fds() -> PrearmedIncomingFds {
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
    /// The build/commit the PARENT expects this process to be, already checked
    /// equal to our own `build_info` by [`take_target_identity`]. Hashing the
    /// agreed value keeps "I am the binary you asked for" as a real property
    /// while making a disagreement an explicit, logged refusal instead of a
    /// poisoned digest surfacing as an unexplained `AdoptionMismatch`.
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
    /// no expectation (an older parent, or the legacy bridge).
    #[must_use]
    pub(crate) fn own() -> Option<Self> {
        Some(Self {
            build: crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
            commit: normalize_commit(crate::build_info::GIT_COMMIT)?,
        })
    }
}

/// Encode the parent's expectation for [`ENV_TARGET`]. A spoofed value can only
/// ever LOSE a handoff: the parent compares the child's proof against its OWN
/// expectation, so a forged target simply produces a digest the parent rejects.
#[must_use]
pub(crate) fn encode_target_identity(build: u64, commit: &str) -> String {
    format!("{build} {}", commit.trim())
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
/// left for the next launch instead). Malformed values answer `false`: the
/// consuming reader is the one that diagnoses them.
#[must_use]
pub(crate) fn target_identity_names_this_build() -> bool {
    let Some(raw) = std::env::var_os(ENV_TARGET) else {
        return false;
    };
    let raw = raw.to_string_lossy();
    let Some((build, _commit)) = raw.trim().split_once(' ') else {
        return false;
    };
    let own = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
    build.parse::<u64>().ok() == Some(own)
}

/// Consume [`ENV_TARGET`] and PROVE this process is the binary the parent
/// authorized. Returns the agreed identity, or `None` after logging exactly
/// which component disagreed — the diagnostic that was missing when every
/// handoff in the field died as a bare `AdoptionMismatch`.
///
/// SAFETY (env): the caller guarantees this runs before any thread spawn,
/// alongside [`take_incoming`].
#[must_use]
pub(crate) fn take_target_identity() -> Option<HandoffTarget> {
    // Read and cleared in ONE critical section: single-threaded launcher (caller
    // contract), and the authority is one-shot by construction.
    let raw = aterm_log::env::take(ENV_TARGET).and_then(|v| v.into_string().ok());
    let own = HandoffTarget::own();
    let Some(raw) = raw else {
        // Older parent: it hashes ITS ticket's target and we hash our own
        // build_info, exactly as before. Unchanged behaviour, no new failure.
        return own;
    };
    let Some(own) = own else {
        eprintln!(
            "aterm-gui: seamless handoff refused: this build reports commit \
             `{}`, which is not a usable identity",
            crate::build_info::GIT_COMMIT
        );
        return None;
    };
    let (build, commit) = raw.trim().split_once(' ')?;
    let Some(expected) = build
        .parse::<u64>()
        .ok()
        .zip(normalize_commit(commit))
        .map(|(build, commit)| HandoffTarget { build, commit })
    else {
        eprintln!("aterm-gui: seamless handoff refused: malformed target identity `{raw}`");
        return None;
    };
    if expected != own {
        eprintln!(
            "aterm-gui: seamless handoff refused: the outgoing process authorized \
             build {} commit {}, but this binary is build {} commit {}",
            expected.build, expected.commit, own.build, own.commit
        );
        return None;
    }
    Some(expected)
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
    }

    /// Publish a proof that was computed before the Commit waiter was spawned.
    /// The child remains readerless afterward; only the separate exact Commit
    /// can release the already-provisioned reader gate.
    #[must_use]
    pub(crate) fn signal_proof(self, proof: AdoptionProof) -> bool {
        write_wire(&self.fd, &proof.to_wire())
    }
}

#[cfg(unix)]
pub(crate) struct CommitReceiver {
    fd: Option<std::os::fd::OwnedFd>,
    wire: Option<std::sync::mpsc::Receiver<Option<[u8; READY_WIRE_LEN]>>>,
    fail_stop: bool,
    parent: AttestedParent,
}

#[cfg(unix)]
impl CommitReceiver {
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
                        if pollfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                            break None;
                        }
                    } else if polled < 0
                        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                    {
                        break None;
                    }
                    // The fail-stop's liveness witness. It no longer ASSUMES the
                    // process tree can answer: a successor launchd owns has ppid
                    // 1 from birth and this loop used to kill it on the first
                    // pass (blocker B1). See [`AttestedParent`] for the property
                    // being preserved — edge at the parent's `exit`, not its
                    // reap, and immune to PID reuse. The probe is one libproc
                    // call per 10ms tick for the bounded life of the overlap,
                    // which is the same cadence the leaked-writer backstop
                    // already required.
                    if !parent.still_alive() {
                        // Close the write-before-exit race with one final
                        // nonblocking data check before fail-stop.
                        pollfd.revents = 0;
                        let final_poll = unsafe { libc::poll(&mut pollfd, 1, 0) };
                        if final_poll > 0 && pollfd.revents & libc::POLLIN != 0 {
                            break read_atomic_wire(&fd);
                        }
                        break None;
                    }
                };
                if received.is_none() && fail_stop {
                    // Parent EOF before exact Commit means this candidate can
                    // never become authoritative. Exit without App/Session
                    // destructors so adopted PTYs remain owned by the parent.
                    unsafe { libc::_exit(74) }
                }
                if send.send(received).is_err() && fail_stop {
                    // The event-loop authority disappeared while the parent was
                    // deciding. Remaining alive readerless is never terminal.
                    unsafe { libc::_exit(74) }
                }
            })
            .ok()?;
        self.wire = Some(wire);
        Some(self)
    }

    #[cfg(test)]
    pub(crate) fn for_test(fd: std::os::fd::OwnedFd) -> Self {
        // Attest the harness's own parent through the real admission path, so
        // fixtures hold the same kind of witness production does. These
        // receivers are `fail_stop: false` and exercise Commit WIRE delivery,
        // not the death edge — that is
        // `leaked_commit_writer_cannot_hide_parent_death`, which uses a real
        // parent it then kills.
        // SAFETY: `getppid` is a side-effect-free libc getter.
        let creator = unsafe { libc::getppid() };
        let parent = attest_handoff_parent(creator, None)
            .expect("unit fixtures need a live parent process — run under `cargo test`");
        Self::raw(fd, false, parent)
            .start_watch()
            .expect("test commit watcher")
    }

    /// Wait for the exact attempt-bound Commit. No PTY reader may be attached
    /// before this succeeds.
    pub(crate) fn receive_commit(&self, expected: AdoptionProof) -> bool {
        self.wire
            .as_ref()
            .expect("commit watch must start before event-loop ownership")
            .recv()
            .ok()
            .flatten()
            .is_some_and(|wire| expected.commit_wire_matches(&wire))
    }

    /// A full but mismatched Commit is as terminal as EOF. Production children
    /// fail-stop destructor-free; unit fixtures can observe the false result.
    pub(crate) fn fail_stop_if_required(&self) {
        if self.fail_stop {
            unsafe { libc::_exit(74) }
        }
    }
}

#[cfg(unix)]
fn read_atomic_wire(fd: &std::os::fd::OwnedFd) -> Option<[u8; READY_WIRE_LEN]> {
    use std::os::fd::AsRawFd as _;
    let mut wire = [0u8; READY_WIRE_LEN];
    loop {
        // Commit is emitted by one fixed <=PIPE_BUF write. A short positive read
        // is therefore malformed authority, not a prefix to wait on indefinitely.
        let read = unsafe { libc::read(fd.as_raw_fd(), wire.as_mut_ptr().cast(), wire.len()) };
        if read == isize::try_from(wire.len()).ok()? {
            return Some(wire);
        }
        if read < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return None;
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
            // down. That is correct only when the successor owns a job of its
            // own; while it is a fork child of ours (see the KNOWN DEFECT note
            // at the `spawn` call in `app_update_handoff::run_handoff_worker`)
            // it survives as a pid-1 orphan holding this dead job's bootstrap
            // context. Guarded by `tests/handoff_launchd_job.rs`.
            //
            // SAFETY: this is the protocol's point of no return. `_exit` skips
            // every App/Session destructor that could SIGHUP the handed-off PTYs.
            unsafe { libc::_exit(0) }
        }
        if wrote < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(CommitError);
    }
}

/// The readiness signal's owned type is UNINHABITED off unix (`Infallible`),
/// so `Option<ReadySignal>` is statically always-`None` there and
/// every match arm handling `Some` compiles away.
///
/// Local empty enums rather than `Infallible` for one reason: the shared consumer in
/// `lib.rs` names `ReadySignal::raw_fd` as a function value, and an inherent method
/// cannot be hung on a foreign type. Uninhabitedness — the property the paragraph
/// above depends on — is identical either way.
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

/// Consume + validate the overlap-handoff readiness fd ([`ENV_READY_FD`]),
/// clearing the env var immediately (the [`take_incoming`] idiom). Fail-closed:
/// any malformed/dead/tty value returns `None` — the worst a spoof can achieve
/// is LOSING the signal (the parked parent times out and resumes); it can never
/// gain anything. The fd's CLOEXEC is re-armed here: it was deliberately
/// cleared to survive the staged-swap re-exec, and from this point no further
/// child may inherit it or the parent's EOF-based crash detection degrades.
///
/// SAFETY (env): the caller guarantees this runs before any thread spawn,
/// alongside `take_incoming`.
#[cfg(unix)]
pub(crate) fn take_ready_fd(
    nonce: Option<String>,
    layout_digest: Option<[u8; 32]>,
    screen_digest: Option<[u8; 32]>,
    target: Option<HandoffTarget>,
    adopted_fds: &[i32],
) -> Option<ReadySignal> {
    // Read and cleared in ONE critical section (caller contract: single-threaded
    // launcher); a one-shot descriptor handoff must never be consumable twice.
    let raw = aterm_log::env::take(ENV_READY_FD).and_then(|v| v.into_string().ok());
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

/// Consume + validate the parent-to-child Commit channel. It is deliberately
/// distinct from the proof/ACK channel so EOF proves the validated parent exited.
#[cfg(unix)]
pub(crate) fn take_commit_fd(
    nonce: Option<String>,
    adopted_fds: &[i32],
    ready_fd: Option<i32>,
    parent: Option<AttestedParent>,
) -> Option<CommitReceiver> {
    // Read and cleared in ONE critical section (caller contract: single-threaded
    // launcher); a one-shot descriptor handoff must never be consumable twice.
    let raw = aterm_log::env::take(ENV_COMMIT_FD).and_then(|v| v.into_string().ok());
    let _nonce = nonce?;
    // Identity was proven when this value was minted (holding an
    // [`AttestedParent`] IS that proof); the open question at admission is only
    // whether that same process is still there to send a Commit. Startup between
    // `prearm_incoming_fds` and here is long enough — it re-runs the whole
    // codesign gate on a boot-apply — for the answer to have changed.
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
pub(crate) fn take_ready_fd(
    _nonce: Option<String>,
    _layout_digest: Option<[u8; 32]>,
    _screen_digest: Option<[u8; 32]>,
    _target: Option<HandoffTarget>,
    _adopted_fds: &[i32],
) -> Option<ReadySignal> {
    None
}

#[cfg(not(unix))]
pub(crate) fn take_commit_fd(
    _nonce: Option<String>,
    _adopted_fds: &[i32],
    _ready_fd: Option<i32>,
    _parent: Option<i32>,
) -> Option<CommitReceiver> {
    None
}

/// Authenticate + CONSUME the incoming handoff, returning the [`Adopted`] live sessions
/// (empty ⇒ no seamless handoff, or it failed auth: start fresh). CLEARS the three env
/// vars and DELETES the manifest file + screen sidecar blobs (single-use), so nothing
/// leaks to shell children and a stale manifest can never be replayed. Must run EARLY in
/// `main`, while single-threaded (env mutation is only sound before any thread spawns).
///
/// SAFETY (env): the caller guarantees this runs before any thread/session spawn.
pub(crate) fn take_incoming() -> IncomingHandoff {
    let ready_env_present = std::env::var_os(ENV_READY_FD).is_some();
    let commit_env_present = std::env::var_os(ENV_COMMIT_FD).is_some();
    let manifest_path = std::env::var(ENV_MANIFEST).ok();
    let env_nonce = std::env::var(ENV_NONCE).ok();
    let fds_wire = std::env::var(ENV_FDS).ok();
    let layout_path = std::env::var(ENV_LAYOUT).ok();
    #[cfg(unix)]
    let mut incoming_pty_guard = fds_wire
        .as_deref()
        .map_or_else(|| IncomingPtyGuard(Vec::new()), IncomingPtyGuard::from_wire);
    // Clear immediately so the vars never reach a spawned shell, regardless of
    // outcome. Single-threaded launcher (caller contract); the multithreaded test
    // binary is the ONE exception, and there every caller serializes env mutation
    // under the test module's `ENV_LOCK` on top of the helper's own lock.
    for key in [ENV_MANIFEST, ENV_FDS, ENV_NONCE, ENV_LAYOUT] {
        aterm_log::env::unset(key);
    }
    let (Some(path), Some(env_nonce), Some(fds_wire)) = (manifest_path, env_nonce, fds_wire) else {
        return IncomingHandoff::default();
    };
    let Some(fds) = decode_fds_bounded(&fds_wire) else {
        return IncomingHandoff::default();
    };
    // The manifest MUST live inside our own 0700 control dir — a path elsewhere is a
    // spoof attempt (a different-uid peer cannot write our dir).
    let Some(dir) = crate::control_auth::socket_dir() else {
        return IncomingHandoff::default();
    };
    if !std::path::Path::new(&path).starts_with(&dir)
        || !manifest_path_matches_nonce(std::path::Path::new(&path), &env_nonce)
    {
        return IncomingHandoff::default();
    }
    let Some(body) = take_regular_capped(
        std::path::Path::new(&path),
        &dir,
        MAX_HANDOFF_MANIFEST_BYTES,
    )
    .and_then(|bytes| String::from_utf8(bytes).ok()) else {
        return IncomingHandoff::default();
    };
    // First line binds the file to the env nonce (written together by us); mismatch ⇒ a
    // stale/foreign file → reject.
    let (file_nonce, toml) = body.split_once('\n').unwrap_or(("", ""));
    if file_nonce != env_nonce {
        return IncomingHandoff::default();
    }
    let Some(manifest) = SessionHandoff::from_toml(toml) else {
        return IncomingHandoff::default();
    };
    let Some(expected) = validated_identities(&manifest, &fds) else {
        return IncomingHandoff::default();
    };
    let expected_ids = expected
        .iter()
        .map(|(local_id, _, _)| *local_id)
        .collect::<Vec<_>>();
    // A v2 attempt is recognizable by its attempt-bound layout env. If its
    // Commit channel is missing or malformed, NEVER reinterpret it as legacy:
    // only an old parent has neither the v2 Commit nor layout env. The legacy
    // bridge may consume the old global restore slot, but only after joining it
    // to this authenticated manifest's exact terminal-id set.
    // MODERN OVERLAP ONLY. The v0.52/v0.53 bridge that used to sit beside this —
    // a parent with neither a Commit channel nor an attempt-bound layout sidecar,
    // reading the mutable GLOBAL restore slot and signalling with one unproven
    // byte — is gone. It existed for parents in the retired two-component
    // lineage, and those builds cannot elect a `vMAJOR.MINOR.PATCH` release, so
    // no such parent can ever hand off to this binary. Keeping it meant every
    // incoming handoff carried a second, weaker acceptance path whose layout was
    // never committed to by the peer.
    if !(commit_env_present && ready_env_present && layout_path.is_some()) {
        return IncomingHandoff::default();
    }
    let (layout, layout_digest) = {
        layout_path
            .and_then(|layout_path| {
                let layout_path = std::path::Path::new(&layout_path);
                let expected_layout = std::path::Path::new(&path).with_extension("layout.toml");
                if !layout_path.starts_with(&dir) || layout_path != expected_layout {
                    return None;
                }
                let wire = take_regular_capped(layout_path, &dir, MAX_HANDOFF_LAYOUT_BYTES)
                    .and_then(|bytes| String::from_utf8(bytes).ok())?;
                // COMMIT TO THE BYTES, NOT TO THE PARSE. `layout` below is only
                // used to rebuild panes; the digest the parent will compare
                // against is a pure function of this exact wire.
                let digest = layout_wire_digest(&wire)?;
                let layout = crate::restore::RestoreManifest::from_toml(&wire)
                    .filter(|layout| layout.covers_exact_seamless_ids(&expected_ids))?;
                Some((layout, digest))
            })
            .map_or((None, None), |(layout, digest)| {
                (Some(layout), Some(digest))
            })
    };
    // Both direct old exec and one-channel overlap require the legacy global
    // layout to join exactly to the authenticated PTY identities. Only the
    // overlap shape emits the old one-byte ACK later.
    if layout.is_none() {
        return IncomingHandoff::default();
    }
    let manifest_path = std::path::Path::new(&path);
    let Some(manifest_stem) = manifest_path.file_stem().and_then(|stem| stem.to_str()) else {
        return IncomingHandoff::default();
    };
    let mut remaining_grid_bytes = MAX_HANDOFF_AGGREGATE_GRID_BYTES;
    let mut used_grid_cells = 0_u64;
    let adopted = manifest
        .sessions
        .iter()
        .map(|rec| {
            let (_, fd, pid) = expected
                .iter()
                .find(|(local_id, _, _)| *local_id == rec.local_id)
                .copied()?;
            if !aterm_pty::fd_is_tty(fd) {
                return None;
            }
            // The outgoing parent clears CLOEXEC only in this process's pre-exec
            // child image. Re-arm before ANY child/session spawn; failure rejects
            // this adoption so no later subprocess can inherit the PTY master.
            aterm_pty::set_cloexec(fd, true).ok()?;
            // SCREEN CARRY is mandatory and exact-path bound. Modern grids are
            // canonical visible-only streams of precisely `rows` records. The
            // v0.52 is accepted only with its no-history witness (screen schema
            // absent/0 and exactly `rows` records); its meta omitted the viewport
            // offset, so a history-bearing payload cannot prove live-bottom state.
            // Alt meta/blob presence must agree.
            let sc = rec.screen.as_ref()?;
            let meta = parse_checkpoint_meta(sc)?;
            let grid_cap = checkpoint_grid_cap(&meta)?;
            if admit_checkpoint_dimensions(
                &mut used_grid_cells,
                meta.rows,
                meta.cols,
                meta.history_lines,
                meta.alt_cursor.is_some(),
            )? != grid_cap
            {
                return None;
            }
            let grid_path =
                manifest_path.with_file_name(format!("{manifest_stem}.s{}.grid", rec.local_id));
            if std::path::Path::new(&sc.grid_file) != grid_path {
                return None;
            }
            let wire_cap = grid_cap;
            let grid = take_grid_capped(&grid_path, &dir, wire_cap, &mut remaining_grid_bytes)?;
            let grid =
                normalize_incoming_checkpoint_grid(grid, meta.rows, meta.cols, meta.history_lines)?;
            let alt_grid = match (&meta.alt_cursor, sc.alt_grid_file.as_deref()) {
                (None, None) => None,
                (Some(_), Some(encoded_path)) => {
                    let alt_path = manifest_path
                        .with_file_name(format!("{manifest_stem}.s{}.altgrid", rec.local_id));
                    if std::path::Path::new(encoded_path) != alt_path {
                        return None;
                    }
                    let bytes =
                        take_grid_capped(&alt_path, &dir, wire_cap, &mut remaining_grid_bytes)?;
                    // The alt screen never carries history.
                    Some(normalize_incoming_checkpoint_grid(
                        bytes, meta.rows, meta.cols, 0,
                    )?)
                }
                _ => return None,
            };
            let checkpoint = meta.into_checkpoint(grid, alt_grid);
            Some(Adopted {
                // Carry the outgoing pool id so the boot re-adopts this shell into its
                // original pane (the restore manifest's leaf carries the same id).
                local_id: rec.local_id,
                master: fd,
                pid,
                // Preserve the fabric SID so `aterm-ctl @<sid>` still resolves the session
                // after the update; a fresh nonce (edge authority resets on the swap —
                // acceptable, cross-session edges are re-established on demand).
                sid: SessionId::new(rec.sid.clone()),
                nonce: LaunchNonce::generate(),
                checkpoint: Some(checkpoint),
            })
        })
        .collect::<Option<Vec<_>>>();
    let Some(adopted) = adopted else {
        return IncomingHandoff::default();
    };
    // COMMIT TO THE CARRIED BYTES. `checkpoint.grid`/`alt_grid` are the sidecar
    // blobs VERBATIM (`normalize_incoming_checkpoint_grid` validates canonicality
    // and returns its input unchanged), and `rec.screen.meta` is the parent's
    // JSON string unparsed — so this hashes exactly what `write_outgoing` hashed.
    // Re-deriving the meta from `CheckpointMeta::from_checkpoint(&rebuilt)` (the
    // old behaviour) required the NEW binary's `CheckpointMeta` serde shape to be
    // byte-identical to the OLD binary's: one added field broke every handoff.
    let Some(mut wire_entries) = adopted
        .iter()
        .map(|item| {
            let checkpoint = item.checkpoint.as_ref()?;
            let carry = manifest
                .sessions
                .iter()
                .find(|rec| rec.local_id == item.local_id)?
                .screen
                .as_ref()?;
            Some(ScreenWireEntry {
                local_id: item.local_id,
                meta: carry.meta.as_bytes(),
                grid: &checkpoint.grid,
                alt_grid: checkpoint.alt_grid.as_deref(),
            })
        })
        .collect::<Option<Vec<_>>>()
    else {
        return IncomingHandoff::default();
    };
    let Some(screen_digest) = screen_wire_digest(&mut wire_entries) else {
        return IncomingHandoff::default();
    };
    drop(wire_entries);
    #[cfg(unix)]
    incoming_pty_guard.transfer_all();
    IncomingHandoff {
        adopted,
        window: manifest.window,
        nonce: Some(env_nonce),
        layout,
        layout_digest,
        screen_digest: Some(screen_digest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restore::{
        RestoreManifest, RestoredSplitTree, RestoredTab, RestoredView, TerminalLeafRestore,
        WindowLayout,
    };
    use crate::session_store::SessionRecord;
    use std::sync::{Mutex, PoisonError};

    /// Serializes ALL env mutation in this module. `set_var`/`remove_var` are
    /// process-global and the test binary runs hundreds of tests on parallel
    /// threads, so "single-threaded test" is NEVER a true claim here — every
    /// env-touching test must hold this lock for its WHOLE body. Acquire via
    /// `unwrap_or_else(PoisonError::into_inner)`: a failed (panicked) test must
    /// not poison the lock for the other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// RAII save/RESTORE of one env var: captures the prior value on creation
    /// and puts it BACK (not merely removes it) on drop — including the unwind
    /// of an early failed assert — so unrelated tests that resolve the runtime
    /// dir keep the environment they started with. Must only live while
    /// `ENV_LOCK` is held: declare it AFTER the lock guard so it drops FIRST.
    struct RestoreVar {
        key: &'static str,
        prior: Option<std::ffi::OsString>,
    }
    impl RestoreVar {
        fn new(key: &'static str) -> Self {
            Self {
                key,
                prior: std::env::var_os(key),
            }
        }
    }
    impl Drop for RestoreVar {
        fn drop(&mut self) {
            // Mutation is serialized by ENV_LOCK, which the enclosing test holds
            // for our whole lifetime, AND by the helper's own process-global lock.
            // Readers elsewhere in the process may still observe the transient
            // value — acceptable because no non-seamless test depends on these
            // vars and the prior value is what we reinstate here.
            match &self.prior {
                Some(v) => aterm_log::env::set(self.key, v),
                None => aterm_log::env::unset(self.key),
            }
        }
    }

    fn exact_legacy_layout(ids: &[u64]) -> RestoreManifest {
        RestoreManifest::new(vec![WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            tabs: Vec::new(),
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: Some(0),
            restored_tabs: ids
                .iter()
                .map(|id| RestoredTab {
                    root: RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                        cwd: None,
                        title: format!("session-{id}"),
                        profile: None,
                        local_id: Some(*id),
                        user_title: None,
                        description: None,
                        icon: None,
                        role: None,
                        attention: None,
                    })),
                    focused_path: Vec::new(),
                    zoomed: false,
                })
                .collect(),
        }])
    }

    #[test]
    fn random_nonce_is_hex_and_unique() {
        let a = random_nonce();
        let b = random_nonce();
        assert_eq!(a.len(), 32, "16 bytes → 32 hex chars");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "two draws differ (CSPRNG)");
    }

    /// EXHAUSTIVE over all 256 env-presence combinations, because the cost of a
    /// wrong answer here is not a refused handoff: a successor that accepts a
    /// MIXED shape would read an fd NUMBER out of an environment whose process
    /// inherited no descriptors, and adopt whatever LaunchServices left at that
    /// index as a terminal master.
    #[test]
    fn protocol_shape_table_accepts_exactly_the_two_legal_forms() {
        let shape = |bits: u16| HandoffEnvPresence {
            manifest: bits & 1 != 0,
            fds: bits & 2 != 0,
            nonce: bits & 4 != 0,
            layout: bits & 8 != 0,
            ready: bits & 16 != 0,
            commit: bits & 32 != 0,
            rendezvous: bits & 64 != 0,
            claim: bits & 128 != 0,
        };
        for bits in 0u16..256 {
            let manifest = bits & 1 != 0;
            let fds = bits & 2 != 0;
            let nonce = bits & 4 != 0;
            let layout = bits & 8 != 0;
            let ready = bits & 16 != 0;
            let commit = bits & 32 != 0;
            let rendezvous = bits & 64 != 0;
            let claim = bits & 128 != 0;
            let got = handoff_is_modern_overlap(shape(bits));
            let base = manifest && nonce && layout;
            let inherited = base && fds && ready && commit && !rendezvous && !claim;
            let out_of_band = cfg!(target_os = "macos")
                && base
                && rendezvous
                && claim
                && !fds
                && !ready
                && !commit;
            assert_eq!(got, inherited || out_of_band, "shape bits {bits:08b}");
        }
        // The two forms are disjoint and neither is empty, so the disjunction
        // above is not quietly satisfied by one of them being unreachable.
        assert!(handoff_is_modern_overlap(shape(0b0011_1111)));
        assert_eq!(
            handoff_is_modern_overlap(shape(0b1100_1101)),
            cfg!(target_os = "macos")
        );
        assert!(
            !handoff_is_modern_overlap(shape(0b1111_1111)),
            "a MIXED shape is a forgery or a bug, never a handoff"
        );
    }

    // Immutable bytes emitted by v0.52's `serialize_lines`. Plain lines use its
    // v3 record form: version, flags, LE content length, content, no attrs, zero
    // links. Screen metadata in that release had no `schema` field.
    // Actual v0.52 producer semantics at display_offset=1: it first serialized
    // all history, then `Grid::row(r)` appended the scrolled viewport
    // (`history-1`, `visible-0`) instead of the live (`visible-0`, `visible-1`).
    // Captured from v0.52's CheckpointMeta serializer for a terminal widened to
    // 24 columns, given a custom stop at column 22, then narrowed to 20 before
    // `visible-v052`. It has neither later saved-cursor field; the enclosing
    // ScreenCarry also had no schema key. The four retained off-width entries
    // are the real resize semantics the compatibility parser must preserve.

    #[test]
    fn a_carried_checkpoint_round_trips_its_scrollback_and_bounds_it() {
        use aterm_core::terminal::Terminal;

        // A terminal with far more history than any bound would carry.
        let mut t = Terminal::new(4, 20);
        for i in 0..400 {
            t.process(format!("line{i}\r\n").as_bytes());
        }

        // Visible-only is the old behaviour and must still be expressible.
        let visible = t.checkpoint_visible().expect("Ground");
        assert_eq!(visible.history_lines, 0);
        assert!(checkpoint_grid_is_canonical(&visible.grid, 4, 20, 0));

        // A carried checkpoint declares its history and validates against it.
        let carried = t
            .checkpoint_carry(MAX_HANDOFF_HISTORY_LINES as usize)
            .expect("Ground");
        assert_eq!(
            carried.history_lines, MAX_HANDOFF_HISTORY_LINES,
            "a deep ring fills the bound exactly"
        );
        assert!(
            checkpoint_grid_is_canonical(
                &carried.grid,
                carried.rows,
                carried.cols,
                carried.history_lines
            ),
            "the consumer must accept the shape the producer emits"
        );
        // ...and rejects it when told the wrong history count, so the declared
        // length is genuinely load-bearing rather than decorative.
        assert!(!checkpoint_grid_is_canonical(
            &carried.grid,
            carried.rows,
            carried.cols,
            0
        ));

        // The bound is a real ceiling: a hostile meta cannot exceed it.
        assert!(
            dimension_grid_cap(4, 20, MAX_HANDOFF_HISTORY_LINES + 1).is_none(),
            "history beyond the ceiling is refused before any decode"
        );

        // A shallow ring carries only what it has, not the whole bound.
        let mut small = Terminal::new(4, 20);
        small.process(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        let shallow = small
            .checkpoint_carry(MAX_HANDOFF_HISTORY_LINES as usize)
            .expect("Ground");
        assert!(shallow.history_lines < MAX_HANDOFF_HISTORY_LINES);
        assert!(checkpoint_grid_is_canonical(
            &shallow.grid,
            shallow.rows,
            shallow.cols,
            shallow.history_lines
        ));

        // THE POINT: restoring a carried checkpoint really does bring history back.
        let restored =
            Terminal::from_checkpoint(&carried, aterm_core::terminal::HostBindings::none());
        assert_eq!(
            restored.grid().scrollback_lines(),
            MAX_HANDOFF_HISTORY_LINES as usize,
            "a seamless update must not truncate the tab to one screen"
        );
    }

    #[test]
    fn modern_wide_narrow_checkpoint_preserves_full_tab_vector_and_rejects_bad_lengths() {
        let mut source = aterm_core::terminal::Terminal::new(6, 120);
        source.process(b"\x1b[3g\x1b[1;97H\x1bH");
        source.resize(6, 40);
        let checkpoint = source.checkpoint_visible().expect("Ground modern capture");
        let meta = CheckpointMeta::from_checkpoint(&checkpoint);
        assert_eq!(meta.cursor.tab_stops.len(), 120);
        assert!(meta.cursor.tab_stops[96]);
        assert!(checkpoint_meta_is_bounded(&meta));

        let carry = ScreenCarry {
            schema: ScreenCarry::SCHEMA,
            meta: serde_json::to_string(&meta).unwrap(),
            grid_file: "unused-in-parser-test".to_string(),
            alt_grid_file: None,
        };
        let parsed = parse_checkpoint_meta(&carry).expect("modern strict meta");
        let rebuilt_checkpoint = parsed.into_checkpoint(checkpoint.grid.clone(), None);
        let mut restored = aterm_core::terminal::Terminal::from_checkpoint(
            &rebuilt_checkpoint,
            aterm_core::terminal::HostBindings::none(),
        );
        source.resize(6, 120);
        restored.resize(6, 120);
        for terminal in [&mut source, &mut restored] {
            terminal.process(b"\x1b[1;90H\t");
        }
        assert_eq!(source.cursor().col, 96);
        assert_eq!(restored.cursor(), source.cursor());
        assert_eq!(restored.grid().tab_stops(), source.grid().tab_stops());

        let mut short = meta.clone();
        short.cursor.tab_stops.truncate(39);
        assert!(!checkpoint_meta_is_bounded(&short));
        let mut oversized = meta;
        oversized.cursor.tab_stops = vec![false; usize::from(aterm_core::grid::MAX_GRID_COLS) + 1];
        assert!(
            !checkpoint_meta_is_bounded(&oversized),
            "oversize vector is rejected before restore/allocation growth"
        );
    }

    #[test]
    fn decoded_cell_budget_accepts_exact_max_and_rejects_max_plus_one() {
        let mut used = 0;
        assert!(
            admit_checkpoint_dimensions(&mut used, 256, 128, 0, false).is_some(),
            "256×128 is the exact per-grid cell ceiling"
        );
        assert_eq!(used, MAX_HANDOFF_GRID_CELLS);
        let before = used;
        assert!(
            admit_checkpoint_dimensions(&mut used, 257, 128, 0, false).is_none(),
            "one row beyond the per-grid ceiling is rejected before capture"
        );
        assert_eq!(used, before, "failed admission is transactional");

        let mut aggregate = MAX_HANDOFF_AGGREGATE_GRID_CELLS;
        assert!(
            admit_checkpoint_dimensions(&mut aggregate, 1, 1, 0, false).is_none(),
            "aggregate max+1 cell is rejected"
        );
        assert_eq!(aggregate, MAX_HANDOFF_AGGREGATE_GRID_CELLS);
    }

    /// REGRESSION (the update that would not install). A real machine had a staged
    /// build refuse to apply 38 consecutive times with a capture-admission failure,
    /// because carrying scrollback made an ORDINARY session set exceed the aggregate
    /// cell budget — and the capture loop treated that refusal as fatal instead of
    /// dropping the history it was carrying.
    ///
    /// This pins the arithmetic that makes the degrade load-bearing rather than
    /// theoretical: at one real window geometry, carried history admits strictly
    /// fewer sessions than visible-only, so the degrade below is exercised.
    ///
    /// RETIRED RUNG. This used to also assert `with_history <= 4`, with a note that
    /// raising the aggregate should retire that bound deliberately. It has been
    /// retired: the "4" was never a property, it was the symptom. The aggregate was
    /// 128 Ki cells, so four ordinary panes exhausted it and the FIFTH could not be
    /// admitted even visible-only — the update simply did not apply. The aggregate
    /// is now 4 Mi cells and the producer reserves every session's mandatory
    /// visible+alt cells before anyone spends on scrollback, so that pool is
    /// admitted outright — `app_update_handoff`'s
    /// `a_heavy_pool_is_admitted_even_when_it_must_drop_history` pins that. What
    /// survives here is what the rung stood in for: history still costs real
    /// aggregate, so the degrade still upholds `checkpoint_carry`'s documented
    /// "the failure mode is *less scrollback*, never *the update did not apply*".
    #[test]
    fn carried_history_can_exhaust_the_aggregate_that_visible_only_admits() {
        // The reported window: 49 rows x 110 cols, several tabs/panes.
        let (rows, cols) = (49u16, 110u16);
        let history = MAX_HANDOFF_HISTORY_LINES;

        let admits = |history: u32| {
            let mut used = 0u64;
            let mut n = 0;
            while admit_checkpoint_dimensions(&mut used, rows, cols, history, true).is_some() {
                n += 1;
                assert!(n < 10_000, "admission must terminate");
            }
            n
        };

        let with_history = admits(history);
        let visible_only = admits(0);

        assert!(
            with_history < visible_only,
            "carrying history must cost aggregate budget \
             (with={with_history}, without={visible_only})"
        );
        assert!(
            visible_only >= with_history * 2,
            "dropping history must buy back real headroom, not a rounding error \
             (with={with_history}, without={visible_only})"
        );

        // THE FIX, in miniature: the exact session that is refused WITH history must
        // be admitted WITHOUT it against the SAME aggregate — otherwise degrading
        // could not rescue the handoff. This also pins the transactional property the
        // degrade relies on: a refused admission leaves the aggregate untouched, so
        // the re-probe is exact rather than approximate.
        let mut used = 0u64;
        for _ in 0..with_history {
            assert!(admit_checkpoint_dimensions(&mut used, rows, cols, history, true).is_some());
        }
        let saturated = used;
        assert!(
            admit_checkpoint_dimensions(&mut used, rows, cols, history, true).is_none(),
            "the next session must be the one that trips the budget"
        );
        assert_eq!(used, saturated, "a refused admission is transactional");
        assert!(
            admit_checkpoint_dimensions(&mut used, rows, cols, 0, true).is_some(),
            "the same session must be admissible visible-only — this is precisely \
             the retry that turns 'the update did not apply' back into 'less scrollback'"
        );
    }

    #[test]
    fn max_admitted_visible_capture_meets_release_park_budget() {
        let (rows, cols) = (256u16, 128u16);
        let mut used = 0;
        assert!(admit_checkpoint_dimensions(&mut used, rows, cols, 0, false).is_some());
        let mut terminal = aterm_core::terminal::Terminal::new(rows, cols);
        let mut input = Vec::new();
        input.reserve_exact(usize::from(rows) * usize::from(cols) * 6);
        for index in 0..usize::from(rows) * usize::from(cols) {
            if index % 2 == 0 {
                input.extend_from_slice(b"\x1b[31mA");
            } else {
                input.extend_from_slice(b"\x1b[32mB");
            }
        }
        terminal.process(&input);
        let started = std::time::Instant::now();
        let checkpoint = terminal
            .checkpoint_visible()
            .expect("fixture ends in parser Ground");
        let elapsed = started.elapsed();
        assert_eq!(checkpoint.rows, rows);
        assert_eq!(checkpoint.cols, cols);
        if !cfg!(debug_assertions) {
            assert!(
                elapsed < std::time::Duration::from_millis(20),
                "max admitted worst-style capture took {elapsed:?}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn malformed_named_fd_is_closed_on_authority_rejection() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore_fds = RestoreVar::new(ENV_FDS);
        let _restore_manifest = RestoreVar::new(ENV_MANIFEST);
        let _restore_nonce = RestoreVar::new(ENV_NONCE);
        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
        // Keep the read endpoint and name the write endpoint. Observing the
        // pipe peer proves that exact open-file description was closed; checking
        // the numeric descriptor after close is racy because another parallel
        // test may immediately reuse the number.
        let (peer, named) = (pipe[0], pipe[1]);
        assert!(
            peer >= 3 && named >= 3,
            "handoff descriptors are never stdio"
        );
        // This test runs beside process-spawning tests. Prevent a concurrent
        // posix_spawn from inheriting a second writer that would temporarily
        // mask the EOF produced by the exact close under test.
        aterm_pty::set_cloexec(peer, true).expect("read peer cloexec");
        aterm_pty::set_cloexec(named, true).expect("named writer cloexec");
        let peer_flags = unsafe { libc::fcntl(peer, libc::F_GETFL) };
        assert!(peer_flags >= 0, "read peer flags");
        assert_eq!(
            unsafe { libc::fcntl(peer, libc::F_SETFL, peer_flags | libc::O_NONBLOCK) },
            0,
            "read peer nonblocking"
        );
        let mut byte = [0u8; 1];
        assert_eq!(
            unsafe { libc::read(peer, byte.as_mut_ptr().cast(), byte.len()) },
            -1,
            "negative control: a live named writer leaves the empty peer pending"
        );
        assert!(matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK
        ));
        // The descriptor position is syntactically valid, while both identity
        // and pid invalidate the authority. Duplicate mentions must close once.
        aterm_log::env::unset(ENV_MANIFEST);
        aterm_log::env::unset(ENV_NONCE);
        aterm_log::env::set(ENV_FDS, format!("bad={named}:badpid,bad={named}:badpid"));
        assert!(take_incoming().adopted.is_empty());
        // A process forked in the tiny pipe-create→CLOEXEC window can retain a
        // duplicate until it execs. POLLHUP is still the exact open-file-
        // description witness, while this bounded wait removes scheduler luck
        // from the parallel all-target gate.
        let mut pfd = libc::pollfd {
            fd: peer,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let polled = loop {
            let result = unsafe { libc::poll(&raw mut pfd, 1, 2_000) };
            if result < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            break result;
        };
        assert_eq!(
            polled, 1,
            "closed named writer must make its peer readable/hung up"
        );
        assert_ne!(
            pfd.revents & libc::POLLHUP,
            0,
            "peer wake must be the named writer's EOF"
        );
        assert_eq!(
            unsafe { libc::read(peer, byte.as_mut_ptr().cast(), byte.len()) },
            0,
            "every syntactically named invalid-authority endpoint reaches peer EOF"
        );
        aterm_pty::close_fd(peer);
    }

    #[test]
    #[cfg(unix)]
    fn invalid_prearm_alias_closes_once_clears_authority_and_cannot_close_reused_fd() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore_manifest = RestoreVar::new(ENV_MANIFEST);
        let _restore_fds = RestoreVar::new(ENV_FDS);
        let _restore_nonce = RestoreVar::new(ENV_NONCE);
        let _restore_layout = RestoreVar::new(ENV_LAYOUT);
        let _restore_ready = RestoreVar::new(ENV_READY_FD);
        let _restore_commit = RestoreVar::new(ENV_COMMIT_FD);

        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
        let (aliased, peer) = (pipe[0], pipe[1]);
        // This is an otherwise recognizable v0.52 overlap shape, but READY
        // aliases the PTY authority. A single fd cannot have two owners.
        aterm_log::env::set(ENV_MANIFEST, "/tmp/not-consumed-by-prearm");
        aterm_log::env::set(ENV_FDS, format!("7={aliased}:4242"));
        aterm_log::env::set(ENV_NONCE, "0123456789abcdef0123456789abcdef");
        aterm_log::env::unset(ENV_LAYOUT);
        aterm_log::env::set(ENV_READY_FD, aliased.to_string());
        aterm_log::env::unset(ENV_COMMIT_FD);

        let prearmed = prearm_incoming_fds();
        assert!(prearmed.rejects_boot());
        assert!(prearmed.final_exec_fds().is_empty());
        assert_eq!(unsafe { libc::fcntl(aliased, libc::F_GETFD) }, -1);
        for key in [
            ENV_MANIFEST,
            ENV_FDS,
            ENV_NONCE,
            ENV_LAYOUT,
            ENV_READY_FD,
            ENV_COMMIT_FD,
        ] {
            assert!(std::env::var_os(key).is_none(), "{key} cleared");
        }

        // Reuse the exact numeric slot for an unrelated resource. A later
        // authority parse sees no stale environment and therefore cannot close
        // the replacement (the double-close/fd-reuse regression).
        let source = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
        assert!(source >= 0, "open replacement");
        if source != aliased {
            assert_eq!(unsafe { libc::dup2(source, aliased) }, aliased, "reuse fd");
            aterm_pty::close_fd(source);
        }
        assert!(take_incoming().adopted.is_empty());
        assert!(
            unsafe { libc::fcntl(aliased, libc::F_GETFD) } >= 0,
            "stale handoff must not close a reused descriptor"
        );
        aterm_pty::close_fd(aliased);
        aterm_pty::close_fd(peer);
    }

    /// `final_exec_env` is authority-gated exactly like `final_exec_fds`: only
    /// a VALID prearm with a validated parent yields the restoration pair, so
    /// a malformed handoff can never smuggle a parent-authority variable onto
    /// the exec image.
    #[test]
    #[cfg(unix)]
    fn final_exec_env_requires_valid_parent_authority() {
        let attested = AttestedParent {
            pid: 4242,
            witness: ParentWitness::ForkLink,
        };
        let valid = PrearmedIncomingFds {
            fds: Vec::new(),
            recognizable: true,
            valid: true,
            parent: Some(attested),
        };
        assert_eq!(
            valid.final_exec_env(),
            vec![(
                std::ffi::OsString::from(ENV_PARENT_PID),
                std::ffi::OsString::from("4242"),
            )]
        );
        // A birth witness republishes the record too, so the re-exec'd image can
        // re-attest without a parent link — the property a LaunchServices
        // successor depends on.
        let with_birth = PrearmedIncomingFds {
            fds: Vec::new(),
            recognizable: true,
            valid: true,
            parent: Some(AttestedParent {
                pid: 4242,
                witness: ParentWitness::Birth(ProcessBirth {
                    seconds: 17,
                    microseconds: 42,
                }),
            }),
        };
        assert_eq!(
            with_birth.final_exec_env(),
            vec![
                (
                    std::ffi::OsString::from(ENV_PARENT_PID),
                    std::ffi::OsString::from("4242"),
                ),
                (
                    std::ffi::OsString::from(ENV_PARENT_BIRTH),
                    std::ffi::OsString::from("17.42"),
                ),
            ]
        );
        let invalid = PrearmedIncomingFds {
            fds: Vec::new(),
            recognizable: true,
            valid: false,
            parent: Some(attested),
        };
        assert!(invalid.final_exec_env().is_empty());
        let parentless = PrearmedIncomingFds {
            fds: Vec::new(),
            recognizable: false,
            valid: true,
            parent: None,
        };
        assert!(parentless.final_exec_env().is_empty());
    }

    /// The birth record crosses a process boundary as text, so its encoding is
    /// protocol. A value that does not round-trip would silently demote every
    /// modern handoff to the legacy witness; a malformed one must be refused
    /// outright rather than read as "absent" (see the `birth_present` arm of
    /// `prearm_incoming_fds`).
    #[test]
    #[cfg(unix)]
    fn process_birth_wire_round_trips_and_rejects_malformed_values() {
        let birth = ProcessBirth {
            seconds: 1_784_100_000,
            microseconds: 7,
        };
        assert_eq!(birth.to_wire(), "1784100000.7");
        assert_eq!(ProcessBirth::from_wire(&birth.to_wire()), Some(birth));
        assert_eq!(ProcessBirth::from_wire("  1784100000.7  "), Some(birth));
        for bad in [
            "",
            "1784100000",
            "1784100000.",
            ".7",
            "a.b",
            "-1.7",
            "1.2.3",
        ] {
            assert_eq!(ProcessBirth::from_wire(bad), None, "malformed: {bad:?}");
        }
    }

    /// IDENTITY, the half of parental attestation that must NOT weaken when
    /// `getppid()` stops being the liveness probe (blocker B1).
    ///
    /// While this process has a live creator, the kernel parent link is the
    /// authority and an environment-supplied claim may only AGREE with it. This
    /// is the arm that refuses a stale `ATERM_HANDOFF_*` block that leaked into
    /// a user shell which later runs `aterm`: such a process has a live creator
    /// (the shell), so no published detail can make an unrelated pid pass.
    #[test]
    #[cfg(unix)]
    fn attestation_refuses_any_pid_that_is_not_our_live_creator() {
        // SAFETY: `getppid` is a side-effect-free libc getter.
        let creator = unsafe { libc::getppid() };
        assert!(creator > 1, "the test harness must have a real parent");
        let ours = libc::pid_t::try_from(std::process::id()).expect("our own pid");

        assert!(
            attest_handoff_parent_from(creator, None, creator).is_some(),
            "the live creator itself is always attestable"
        );
        assert_eq!(
            attest_handoff_parent_from(ours, None, creator),
            None,
            "a live creator that is not the claimed pid refuses the claim"
        );
        for claimed in [0, 1] {
            assert_eq!(
                attest_handoff_parent_from(claimed, None, claimed),
                None,
                "pid {claimed} is what reparenting produces, never a parent"
            );
        }
    }

    /// BLOCKER B1, stated as a test: a successor with NO live creator — which is
    /// what a LaunchServices-launched process is from birth, `getppid() == 1`
    /// before it runs its first instruction — is admissible exactly when the
    /// outgoing process published its own kernel birth record, and not
    /// otherwise. The old `getppid()`-only predicate refused it unconditionally
    /// and then fail-stopped it, which is why the launchd-job repair could not
    /// start.
    ///
    /// The stand-in parent is a real spawned process rather than this one, so
    /// the "wrong record at a live pid" case below is the pid-reuse scenario
    /// itself: a live process sitting at the attested pid that is NOT the
    /// attested process.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_creatorless_successor_is_admitted_only_by_a_published_birth_record() {
        const NO_LIVE_CREATOR: libc::pid_t = 1;
        let mut stand_in = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the stand-in parent");
        let pid = libc::pid_t::try_from(stand_in.id()).expect("stand-in pid");
        let birth = read_process_birth(pid).expect("a live process has a birth record");
        let ours = libc::pid_t::try_from(std::process::id()).expect("our own pid");
        let not_its_birth = read_process_birth(ours).expect("our own birth record");
        assert_ne!(
            birth, not_its_birth,
            "two processes never share a birth instant"
        );

        assert_eq!(
            attest_handoff_parent_from(pid, Some(birth), NO_LIVE_CREATOR),
            Some(AttestedParent {
                pid,
                witness: ParentWitness::Birth(birth),
            }),
            "a published record the kernel corroborates is a complete witness"
        );
        assert_eq!(
            attest_handoff_parent_from(pid, None, NO_LIVE_CREATOR),
            None,
            "with no creator and nothing published there is no witness at all"
        );
        assert_eq!(
            attest_handoff_parent_from(pid, Some(not_its_birth), NO_LIVE_CREATOR),
            None,
            "a live pid carrying the WRONG birth record is the pid-reuse case, \
             and it must refuse rather than fall back to something weaker"
        );

        stand_in.kill().expect("kill the stand-in parent");
        stand_in.wait().expect("reap the stand-in parent");
        assert_eq!(
            attest_handoff_parent_from(pid, Some(birth), NO_LIVE_CREATOR),
            None,
            "a dead parent is not admissible however well it was described"
        );
    }

    /// LIVENESS, the half that runs for the whole overlap and whose false
    /// "alive" is the dangerous one: it would strand a readerless successor
    /// holding duplicated masters of the user's live shells.
    ///
    /// The witness discriminates by the kernel's birth record, so it survives
    /// the successor not being a child of the process it watches, and a
    /// different process at the same pid does not satisfy it.
    #[test]
    #[cfg(target_os = "macos")]
    fn parent_liveness_tracks_the_process_not_the_pid() {
        let mut watched = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the watched process");
        let pid = libc::pid_t::try_from(watched.id()).expect("watched pid");
        let birth = read_process_birth(pid).expect("a live process has a birth record");
        let attested = AttestedParent {
            pid,
            witness: ParentWitness::Birth(birth),
        };
        assert!(
            attested.still_alive(),
            "the watched process is running and is not this process's parent, \
             which is exactly the situation a launchd-owned successor is in"
        );

        // The same pid, described by a record that belongs to someone else: what
        // a recycled pid looks like from here.
        let ours = libc::pid_t::try_from(std::process::id()).expect("our own pid");
        let our_birth = read_process_birth(ours).expect("our own birth record");
        let impostor = AttestedParent {
            pid,
            witness: ParentWitness::Birth(our_birth),
        };
        assert!(
            !impostor.still_alive(),
            "a live pid alone must never satisfy the liveness witness"
        );

        watched.kill().expect("kill the watched process");
        watched.wait().expect("reap the watched process");
        assert!(
            !attested.still_alive(),
            "the witness must go false when the attested process exits — this is \
             the fail-stop that stops a readerless candidate outliving its parent"
        );
    }

    /// THE ChildDied regression (2026-07-22): a valid OverlapModern prearm
    /// consumes `ATERM_HANDOFF_PARENT_PID` out of the ambient environment
    /// (helpers spawned afterwards must never see it) — but the boot-apply
    /// re-exec must be able to RESTORE that authority onto the successor
    /// image, or the new build's own prearm rejects the inherited handoff and
    /// the parked parent reads EOF. This proves the round trip: prearm is
    /// valid, the ambient variables are gone, and `final_exec_env` carries the
    /// exact pairs the successor's prearm requires.
    #[test]
    #[cfg(unix)]
    fn valid_overlap_prearm_scrubs_ambient_parent_but_restores_it_for_exec() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore_manifest = RestoreVar::new(ENV_MANIFEST);
        let _restore_fds = RestoreVar::new(ENV_FDS);
        let _restore_nonce = RestoreVar::new(ENV_NONCE);
        let _restore_layout = RestoreVar::new(ENV_LAYOUT);
        let _restore_ready = RestoreVar::new(ENV_READY_FD);
        let _restore_commit = RestoreVar::new(ENV_COMMIT_FD);
        let _restore_parent = RestoreVar::new(ENV_PARENT_PID);
        let _restore_birth = RestoreVar::new(ENV_PARENT_BIRTH);

        let mut master_pipe = [0i32; 2];
        let mut ready_pipe = [0i32; 2];
        let mut commit_pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(master_pipe.as_mut_ptr()) }, 0, "pipe");
        assert_eq!(unsafe { libc::pipe(ready_pipe.as_mut_ptr()) }, 0, "pipe");
        assert_eq!(unsafe { libc::pipe(commit_pipe.as_mut_ptr()) }, 0, "pipe");
        let parent = unsafe { libc::getppid() };
        assert!(parent > 1, "test harness must have a real parent");
        aterm_log::env::set(ENV_MANIFEST, "/tmp/not-consumed-by-prearm");
        aterm_log::env::set(ENV_FDS, format!("7={}:4242", master_pipe[0]));
        aterm_log::env::set(ENV_NONCE, "0123456789abcdef0123456789abcdef");
        aterm_log::env::set(ENV_LAYOUT, "/tmp/not-consumed-by-prearm.layout.toml");
        aterm_log::env::set(ENV_READY_FD, ready_pipe[1].to_string());
        aterm_log::env::set(ENV_COMMIT_FD, commit_pipe[0].to_string());
        aterm_log::env::set(ENV_PARENT_PID, parent.to_string());
        aterm_log::env::unset(ENV_PARENT_BIRTH);

        let prearmed = prearm_incoming_fds();
        assert!(
            !prearmed.rejects_boot(),
            "the modern overlap shape is valid"
        );
        assert!(
            !prearmed.final_exec_fds().is_empty(),
            "descriptors survive for the final exec"
        );
        for key in [ENV_PARENT_PID, ENV_PARENT_BIRTH] {
            assert!(
                std::env::var_os(key).is_none(),
                "ambient parent authority ({key}) is scrubbed before any helper can spawn"
            );
        }
        // This handoff was published in the LEGACY shape — pid only, exactly what
        // a pre-0.13 outgoing build sends. Admission still succeeds, because the
        // parent link proves this pid is our creator; and where the kernel offers
        // a birth record the successor CAPTURES it under that proof and hands it
        // forward, so the re-exec'd image no longer needs a parent link either.
        let mut expected = vec![(
            std::ffi::OsString::from(ENV_PARENT_PID),
            std::ffi::OsString::from(parent.to_string()),
        )];
        if let Some(birth) = read_process_birth(parent) {
            expected.push((
                std::ffi::OsString::from(ENV_PARENT_BIRTH),
                std::ffi::OsString::from(birth.to_wire()),
            ));
        }
        assert_eq!(
            prearmed.final_exec_env(),
            expected,
            "the exec image gets the exact validated authority back"
        );

        for fd in [
            master_pipe[0],
            master_pipe[1],
            ready_pipe[0],
            ready_pipe[1],
            commit_pipe[0],
            commit_pipe[1],
        ] {
            aterm_pty::close_fd(fd);
        }
    }

    #[test]
    fn nonce_mismatch_and_missing_env_fail_closed() {
        // Hold the env lock for the whole body: the roundtrip test SETS these
        // vars mid-flight, and take_incoming() itself mutates env on read.
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        // No env at all → empty (fresh start).
        // SAFETY: mutation is serialized by ENV_LOCK (held for this whole test).
        // Readers elsewhere in the process may still observe the removal —
        // acceptable: no non-seamless test depends on ATERM_SEAMLESS_*.
        aterm_log::env::unset(ENV_MANIFEST);
        aterm_log::env::unset(ENV_NONCE);
        aterm_log::env::unset(ENV_FDS);
        assert!(
            take_incoming().adopted.is_empty(),
            "no handoff env → fresh start"
        );
    }

    /// The readiness fd's fail-closed gauntlet: absent, garbage, stdio, and a
    /// TTY all yield `None` (the parked parent just times out + resumes — a
    /// spoof can only LOSE the signal), and the env var is cleared on read so
    /// it never leaks into shell children.
    #[test]
    #[cfg(unix)]
    fn take_ready_fd_fails_closed() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore = RestoreVar::new("ATERM_HANDOFF_READY_FD");
        // All set/unset below: serialized by ENV_LOCK for the whole body.
        aterm_log::env::unset("ATERM_HANDOFF_READY_FD");
        assert!(
            take_ready_fd(
                Some("n".to_string()),
                Some([7; 32]),
                Some([8; 32]),
                HandoffTarget::own(),
                &[],
            )
            .is_none(),
            "absent env → None"
        );
        aterm_log::env::set("ATERM_HANDOFF_READY_FD", "not-a-number");
        assert!(
            take_ready_fd(
                Some("n".to_string()),
                Some([7; 32]),
                Some([8; 32]),
                HandoffTarget::own(),
                &[],
            )
            .is_none(),
            "garbage → None"
        );
        assert!(
            std::env::var_os("ATERM_HANDOFF_READY_FD").is_none(),
            "cleared on read regardless of outcome"
        );
        aterm_log::env::set("ATERM_HANDOFF_READY_FD", "1");
        assert!(
            take_ready_fd(
                Some("n".to_string()),
                Some([7; 32]),
                Some([8; 32]),
                HandoffTarget::own(),
                &[],
            )
            .is_none(),
            "stdio refused"
        );
        // A real PTY master: the is_tty backstop must refuse it (a confused
        // wire must never let the readiness byte land in a shell's input).
        let (mut m, mut s) = (0i32, 0i32);
        // SAFETY: valid out-params; openpty fills them on success.
        let rc = unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty");
        aterm_log::env::set("ATERM_HANDOFF_READY_FD", m.to_string());
        assert!(
            take_ready_fd(
                Some("n".to_string()),
                Some([7; 32]),
                Some([8; 32]),
                HandoffTarget::own(),
                &[],
            )
            .is_none(),
            "a tty is refused"
        );
        aterm_pty::close_fd(m);
        aterm_pty::close_fd(s);
    }

    /// The happy path: a real pipe write-end is adopted, its CLOEXEC is
    /// RE-ARMED (only the fork child's copy was cleared for its one exec; from
    /// here no further child may inherit it), and a byte written through the adopted fd
    /// arrives at the parent's read end — the actual readiness signal.
    #[test]
    #[cfg(unix)]
    fn take_ready_fd_adopts_a_pipe_and_rearms_cloexec() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore = RestoreVar::new("ATERM_HANDOFF_READY_FD");
        let mut fds = [0i32; 2];
        // SAFETY: plain pipe(2) into a valid 2-slot out-array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (fds[0], fds[1]);
        // Simulate the fork child's inherited copy just before exec.
        let _ = aterm_pty::set_cloexec(wr, false);
        // Serialized by ENV_LOCK.
        aterm_log::env::set("ATERM_HANDOFF_READY_FD", wr.to_string());
        let owned = take_ready_fd(
            Some("n".to_string()),
            Some([7; 32]),
            Some([8; 32]),
            HandoffTarget::own(),
            &[],
        )
        .expect("a live pipe fd is adopted");
        assert!(
            std::env::var_os("ATERM_HANDOFF_READY_FD").is_none(),
            "cleared on read"
        );
        // SAFETY: F_GETFD on an fd we own.
        let flags = unsafe { libc::fcntl(owned.raw_fd(), libc::F_GETFD) };
        assert!(
            flags >= 0 && (flags & libc::FD_CLOEXEC) != 0,
            "CLOEXEC re-armed on adoption (EOF discipline)"
        );
        // The adopted fd IS the readiness channel: one byte through it lands
        // at the parent's read end.
        // SAFETY: 1-byte write/read on fds this test owns.
        let w = unsafe { libc::write(owned.raw_fd(), [1u8].as_ptr().cast(), 1) };
        assert_eq!(w, 1, "readiness byte written");
        let mut buf = [0u8; 1];
        let r = unsafe { libc::read(rd, buf.as_mut_ptr().cast(), 1) };
        assert_eq!((r, buf[0]), (1, 1), "parent reads the byte");
        aterm_pty::close_fd(rd);
    }

    /// Rollback hygiene: `discard_outgoing` retires exactly THIS attempt's
    /// artifacts (manifest + sidecars named by our pid + the nonce) and leaves
    /// every other file — another attempt's manifest, unrelated files — alone.
    #[test]
    fn discard_outgoing_removes_only_this_attempts_artifacts() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore_xdg = RestoreVar::new("XDG_RUNTIME_DIR");
        let tmp = std::env::temp_dir().join(format!("aterm-discard-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        // Serialized by ENV_LOCK; restored by `_restore_xdg`.
        aterm_log::env::set("XDG_RUNTIME_DIR", &tmp);
        let dir = crate::control_auth::socket_dir().expect("scratch control dir");
        std::fs::create_dir_all(&dir).ok();
        let pid = std::process::id();
        let ours = [
            dir.join(format!("seamless-{pid}-aaaa.toml")),
            dir.join(format!("seamless-{pid}-aaaa.s0.grid")),
            dir.join(format!("seamless-{pid}-aaaa.s3.altgrid")),
        ];
        let kept = [
            dir.join(format!("seamless-{pid}-bbbb.toml")), // a DIFFERENT attempt
            dir.join("unrelated.txt"),
        ];
        for p in ours.iter().chain(kept.iter()) {
            std::fs::write(p, b"x").expect("seed file");
        }
        discard_outgoing("aaaa");
        for p in &ours {
            assert!(!p.exists(), "retired: {}", p.display());
        }
        for p in &kept {
            assert!(p.exists(), "untouched: {}", p.display());
            let _ = std::fs::remove_file(p);
        }
    }

    const PARENT_DEATH_MODE: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_MODE";
    const PARENT_DEATH_MARKER: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_MARKER";
    const PARENT_DEATH_READY: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_READY";
    const PARENT_DEATH_READ_FD: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_READ_FD";
    const PARENT_DEATH_WRITE_FD: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_WRITE_FD";
    const PARENT_DEATH_EXPECTED_PID: &str = "ATERM_TEST_COMMIT_PARENT_DEATH_EXPECTED_PID";

    /// Subprocess fixture for `leaked_commit_writer_cannot_hide_parent_death`.
    /// Running normally is a no-op; the outer test selects launcher/watcher mode
    /// through command-local environment, never process-global test mutation.
    #[test]
    #[cfg(unix)]
    fn commit_parent_death_helper() {
        use std::os::fd::FromRawFd as _;

        let Ok(mode) = std::env::var(PARENT_DEATH_MODE) else {
            return;
        };
        if mode == "launcher" {
            let marker = std::path::PathBuf::from(
                std::env::var_os(PARENT_DEATH_MARKER).expect("launcher marker"),
            );
            let ready = std::path::PathBuf::from(
                std::env::var_os(PARENT_DEATH_READY).expect("launcher ready marker"),
            );
            let mut raw = [0i32; 2];
            assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0, "commit pipe");
            assert!(aterm_pty::set_cloexec(raw[0], false).is_ok());
            assert!(aterm_pty::set_cloexec(raw[1], false).is_ok());

            let mut watcher = std::process::Command::new(
                std::env::current_exe().expect("current test executable"),
            );
            watcher
                .arg("--exact")
                .arg("seamless::tests::commit_parent_death_helper")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(PARENT_DEATH_MODE, "watcher")
                .env(PARENT_DEATH_READY, &ready)
                .env(PARENT_DEATH_READ_FD, raw[0].to_string())
                .env(PARENT_DEATH_WRITE_FD, raw[1].to_string())
                .env(PARENT_DEATH_EXPECTED_PID, std::process::id().to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let watcher = watcher.spawn().expect("spawn commit watcher fixture");
            std::fs::write(&marker, watcher.id().to_string()).expect("publish watcher pid");

            // Waits on a re-exec of the whole debug TEST BINARY. 2s barely covers its
            // startup on an idle box, and the wait busy-spun on `yield_now()` — pinning a
            // core and starving the very child it waited for. A genuine failure never
            // produces the marker at all, so the bound is a failure bound.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !ready.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "watcher did not arm parent-liveness detection"
                );
                // sleep, not yield_now(): yielding keeps this thread RUNNABLE, so on an
                // oversubscribed box it competes with the very work it is waiting for.
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // Deliberately do not wait: exiting this launcher is the parent-death
            // cut. The watcher inherited BOTH pipe ends, so EOF alone cannot help.
            drop(watcher);
            aterm_pty::close_fd(raw[0]);
            aterm_pty::close_fd(raw[1]);
            return;
        }

        assert_eq!(mode, "watcher");
        let read_fd: i32 = std::env::var(PARENT_DEATH_READ_FD)
            .expect("read fd")
            .parse()
            .expect("numeric read fd");
        let write_fd: i32 = std::env::var(PARENT_DEATH_WRITE_FD)
            .expect("write fd")
            .parse()
            .expect("numeric write fd");
        let expected_parent: libc::pid_t = std::env::var(PARENT_DEATH_EXPECTED_PID)
            .expect("expected parent")
            .parse()
            .expect("numeric parent pid");
        // SAFETY: the launcher passed distinct owned pipe endpoints solely to
        // this exec. Holding the writer is the Darwin CLOEXEC-leak regression.
        let read_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(read_fd) };
        let _leaked_writer = unsafe { std::os::fd::OwnedFd::from_raw_fd(write_fd) };
        // Attest through the real admission path: the launcher is still blocked
        // on our readiness marker, so it is a live creator and the birth record
        // captured here is provably its own. That makes this fixture exercise
        // the SAME witness production is admitted with.
        let parent = attest_handoff_parent(expected_parent, None)
            .expect("the launcher fixture is alive and is this process's parent");
        let receiver = CommitReceiver::raw(read_fd, true, parent)
            .start_watch()
            .expect("start commit parent watch");
        std::fs::write(
            std::env::var_os(PARENT_DEATH_READY).expect("ready marker"),
            b"armed",
        )
        .expect("publish watcher readiness");
        let proof = adoption_proof(
            "parent-death-regression",
            1,
            crate::build_info::GIT_COMMIT,
            &[1; 32],
            &[2; 32],
            &[],
        )
        .expect("test proof");
        let _ = receiver.receive_commit(proof);
        panic!("production watcher must fail-stop when its direct parent exits");
    }

    #[test]
    #[cfg(unix)]
    fn leaked_commit_writer_cannot_hide_parent_death() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let scratch = std::env::temp_dir().join(format!(
            "aterm-parent-death-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&scratch).expect("create parent-death scratch");
        let marker = scratch.join("watcher.pid");
        let ready = scratch.join("watcher.ready");
        let status =
            std::process::Command::new(std::env::current_exe().expect("current test executable"))
                .arg("--exact")
                .arg("seamless::tests::commit_parent_death_helper")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(PARENT_DEATH_MODE, "launcher")
                .env(PARENT_DEATH_MARKER, &marker)
                .env(PARENT_DEATH_READY, &ready)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("run launcher fixture");
        assert!(status.success(), "launcher fixture armed watcher");
        let watcher: libc::pid_t = std::fs::read_to_string(&marker)
            .expect("watcher pid marker")
            .trim()
            .parse()
            .expect("numeric watcher pid");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let gone = unsafe { libc::kill(watcher, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if gone {
                break;
            }
            if std::time::Instant::now() >= deadline {
                unsafe { libc::kill(watcher, libc::SIGKILL) };
                panic!("leaked commit writer hid direct-parent death past 3 seconds");
            }
            // sleep, not yield_now(): yielding keeps this thread RUNNABLE, so on an
            // oversubscribed box it competes with the very work it is waiting for.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::fs::remove_dir_all(&scratch).expect("remove parent-death scratch");
    }

    // ======================================================================
    // THE HAPPY PATH. Until 2026-07 there was no test anywhere that a full
    // seamless handoff COMPLETES — every test proved a way for it to fail
    // closed, and the RFC said in as many words that the models prove safety
    // but "do not prove the happy path completes". In the field it never
    // completed: not once, across three releases. These tests drive the real
    // handshake end to end and assert SUCCESS, plus the three ways it must
    // still fail closed.
    // ======================================================================

    /// One full outgoing-side preparation, ready to be consumed by the child
    /// half of the same process. `layout_wire_override` writes a DIFFERENT byte
    /// string to the layout sidecar than this binary's codec would produce —
    /// standing in for "the outgoing process was a different build".
    #[cfg(unix)]
    struct StagedHandoff {
        manifest_path: std::path::PathBuf,
        nonce: String,
        fds_wire: String,
        expected: AdoptionProof,
        masters: Vec<i32>,
        slaves: Vec<i32>,
        scratch: std::path::PathBuf,
    }

    #[cfg(unix)]
    fn stage_outgoing_handoff(
        label: &str,
        sessions: usize,
        layout_wire_override: Option<&dyn Fn(&str) -> String>,
    ) -> StagedHandoff {
        stage_outgoing_handoff_with(label, sessions, layout_wire_override, None)
    }

    /// As [`stage_outgoing_handoff`], but `meta_wire_override` also rewrites the
    /// `CheckpointMeta` JSON the outgoing process publishes — standing in for
    /// "the outgoing process's meta codec was a different version". The parent's
    /// screen commitment is recomputed over the REWRITTEN bytes, because that is
    /// what a differently-versioned parent would have hashed.
    #[cfg(unix)]
    fn stage_outgoing_handoff_with(
        label: &str,
        sessions: usize,
        layout_wire_override: Option<&dyn Fn(&str) -> String>,
        meta_wire_override: Option<&dyn Fn(&str) -> String>,
    ) -> StagedHandoff {
        let scratch =
            std::env::temp_dir().join(format!("aterm-handoff-e2e-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch");
        // Every caller holds ENV_LOCK for its whole body.
        aterm_log::env::set("XDG_RUNTIME_DIR", &scratch);
        aterm_log::env::set("HOME", &scratch);
        let dir = crate::control_auth::socket_dir().expect("scratch control dir");
        std::fs::create_dir_all(&dir).expect("control dir");

        let mut masters = Vec::new();
        let mut slaves = Vec::new();
        let mut records = Vec::new();
        let mut live = Vec::new();
        let mut screens = Vec::new();
        for index in 0..sessions {
            let (mut master, mut slave) = (0i32, 0i32);
            // SAFETY: valid out-params; openpty fills them on success.
            let rc = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(rc, 0, "openpty {index}");
            masters.push(master);
            slaves.push(slave);
            let local_id = index as u64;
            records.push(SessionRecord {
                local_id,
                sid: format!("s-e2e-{index}"),
                parent: None,
                state: "alive".to_string(),
                title: "zsh".to_string(),
                screen: None,
                user_title: None,
                description: None,
                icon: None,
                role: None,
                attention: None,
            });
            live.push((local_id, master, 4000 + index as i32));
            let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
            terminal.process(
                format!("\x1b[1;38;5;202muser@mac\x1b[0m ~ % session {index}").as_bytes(),
            );
            screens.push((
                local_id,
                terminal.checkpoint_visible().expect("parser is Ground"),
            ));
        }
        let manifest = SessionHandoff {
            schema: SessionHandoff::SCHEMA,
            window: Some(WindowCarry {
                rows: 24,
                cols: 80,
                outer_x: Some(120),
                outer_y: Some(64),
            }),
            sessions: records,
        };
        let fds = HandoffFds {
            entries: live.clone(),
        };
        let outgoing = write_outgoing(&manifest, &fds, &screens, manifest.window)
            .expect("outgoing handoff is written");

        // The PARENT's layout commitment is a pure function of the bytes it
        // writes to the sidecar — `restore::write_to` writes exactly
        // `to_toml()`, and `layout_digest` hashes exactly that.
        let ids = live.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
        let layout = exact_legacy_layout(&ids);
        let layout_wire = layout.to_toml().expect("serialize layout");
        let layout_wire = layout_wire_override.map_or(layout_wire.clone(), |f| f(&layout_wire));
        let layout_path =
            std::path::Path::new(&outgoing.manifest_path).with_extension("layout.toml");
        std::fs::write(&layout_path, layout_wire.as_bytes()).expect("layout sidecar");
        let parent_layout_digest = layout_wire_digest(&layout_wire).expect("parent layout digest");

        // A differently-versioned outgoing process would have published — and
        // hashed — different meta bytes. Rewrite the manifest it just wrote, then
        // take the parent's commitment over exactly those bytes.
        let mutated_metas: Vec<(u64, String)> = meta_wire_override.map_or_else(Vec::new, |f| {
            // Round-trip through the manifest codec rather than patching TOML
            // text: the MANIFEST bytes are not hashed by anyone (only
            // `ScreenCarry::meta` is), so re-serializing it here is sound and
            // immune to how the codec escapes an embedded JSON string.
            // The published file is `<nonce>\n<manifest toml>`.
            let body = std::fs::read_to_string(&outgoing.manifest_path).expect("read manifest");
            let (file_nonce, toml) = body.split_once('\n').expect("nonce header");
            let mut published =
                SessionHandoff::from_toml(toml).expect("parse the published manifest");
            let mut mutated = Vec::new();
            for rec in &mut published.sessions {
                let carry = rec.screen.as_mut().expect("every session carries a screen");
                let next = f(&carry.meta);
                assert_ne!(
                    next, carry.meta,
                    "the override must actually change the meta"
                );
                carry.meta = next.clone();
                mutated.push((rec.local_id, next));
            }
            let republished = format!(
                "{file_nonce}\n{}",
                published.to_toml().expect("reserialize manifest")
            );
            std::fs::write(&outgoing.manifest_path, republished.as_bytes())
                .expect("republish the rewritten manifest");
            mutated
        });
        let parent_screen_digest = if mutated_metas.is_empty() {
            outgoing.screen_digest
        } else {
            let mut entries = screens
                .iter()
                .map(|(local_id, cp)| ScreenWireEntry {
                    local_id: *local_id,
                    meta: mutated_metas
                        .iter()
                        .find(|(id, _)| id == local_id)
                        .map(|(_, meta)| meta.as_bytes())
                        .expect("every session has a rewritten meta"),
                    grid: &cp.grid,
                    alt_grid: cp.alt_grid.as_deref(),
                })
                .collect::<Vec<_>>();
            screen_wire_digest(&mut entries).expect("parent screen digest over the rewritten wire")
        };

        let expected = adoption_proof(
            &outgoing.nonce,
            crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
            crate::build_info::GIT_COMMIT,
            &parent_layout_digest,
            &parent_screen_digest,
            &live,
        )
        .expect("parent expectation");

        StagedHandoff {
            manifest_path: std::path::PathBuf::from(&outgoing.manifest_path),
            nonce: outgoing.nonce,
            fds_wire: outgoing.fds_wire,
            expected,
            masters,
            slaves,
            scratch,
        }
    }

    #[cfg(unix)]
    impl StagedHandoff {
        fn publish_env(&self, ready_write: i32, commit_read: i32, target: Option<String>) {
            let layout_path = self.manifest_path.with_extension("layout.toml");
            // SAFETY: `getppid` is a side-effect-free libc getter.
            let parent_pid = unsafe { libc::getppid() };
            // Serialized by ENV_LOCK, held by the calling test.
            aterm_log::env::set(ENV_MANIFEST, &self.manifest_path);
            aterm_log::env::set(ENV_NONCE, &self.nonce);
            aterm_log::env::set(ENV_FDS, &self.fds_wire);
            aterm_log::env::set(ENV_LAYOUT, &layout_path);
            aterm_log::env::set(ENV_READY_FD, ready_write.to_string());
            aterm_log::env::set(ENV_COMMIT_FD, commit_read.to_string());
            aterm_log::env::set(ENV_PARENT_PID, parent_pid.to_string());
            // The MODERN shape: publish the parent's birth record alongside its
            // pid, exactly as `outgoing_parent_env` does in production, so the
            // happy-path tests drive the witness a LaunchServices successor will
            // depend on rather than only the legacy pid-plus-parent-link one.
            match read_process_birth(parent_pid) {
                Some(birth) => aterm_log::env::set(ENV_PARENT_BIRTH, birth.to_wire()),
                None => aterm_log::env::unset(ENV_PARENT_BIRTH),
            }
            match target {
                Some(value) => aterm_log::env::set(ENV_TARGET, value),
                None => aterm_log::env::unset(ENV_TARGET),
            }
        }

        fn teardown(self) {
            for fd in self.masters.into_iter().chain(self.slaves) {
                aterm_pty::close_fd(fd);
            }
            let _ = std::fs::remove_dir_all(&self.scratch);
        }
    }

    #[cfg(unix)]
    fn pipe_pair(label: &str) -> (i32, i32) {
        let mut fds = [0i32; 2];
        // SAFETY: plain pipe(2) into a valid 2-slot out-array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "{label} pipe");
        (fds[0], fds[1])
    }

    /// Everything the child half holds at the instant it is ready to publish:
    /// the proof it computed, the still-unfired ready channel it will publish
    /// on, and the adopted session set that proof commits to. The three travel
    /// together because a test that inspects one usually has to check it against
    /// another (proof vs. adopted set, or signal-then-commit). The session set is
    /// spelled with the crate-wide [`SessionIdentity`] alias, so the test oracle
    /// and the production signatures it drives cannot drift on field ORDER.
    #[cfg(unix)]
    type ReadyChild = (AdoptionProof, ReadySignal, Vec<SessionIdentity>);

    /// Drive the CHILD half exactly as `run()` does: consume the manifest,
    /// prove the target identity, adopt the ready channel, compute the proof.
    #[cfg(unix)]
    fn child_proof() -> Option<ReadyChild> {
        let incoming = take_incoming();
        if incoming.adopted.is_empty() {
            return None;
        }
        let target = take_target_identity()?;
        let adopted_fds = incoming
            .adopted
            .iter()
            .map(|adopted| adopted.master)
            .collect::<Vec<_>>();
        let ready = take_ready_fd(
            incoming.nonce.clone(),
            incoming.layout_digest,
            incoming.screen_digest,
            Some(target),
            &adopted_fds,
        )?;
        let adopted = incoming
            .adopted
            .iter()
            .map(|item| (item.local_id, item.master, item.pid))
            .collect::<Vec<_>>();
        let proof = ready.proof(&adopted)?;
        Some((proof, ready, adopted))
    }

    /// THE TEST THAT WAS MISSING. Parent prepares → child consumes, adopts and
    /// proves → the two proofs MATCH → the parent's Commit wire is accepted by
    /// the child's own `commit_wire_matches`. Multi-session so a subset or a
    /// cross-wire cannot pass.
    #[test]
    #[cfg(unix)]
    fn a_full_seamless_handoff_completes_and_commits() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore = [
            RestoreVar::new("XDG_RUNTIME_DIR"),
            RestoreVar::new("HOME"),
            RestoreVar::new(ENV_MANIFEST),
            RestoreVar::new(ENV_NONCE),
            RestoreVar::new(ENV_FDS),
            RestoreVar::new(ENV_LAYOUT),
            RestoreVar::new(ENV_TARGET),
            RestoreVar::new(ENV_READY_FD),
            RestoreVar::new(ENV_COMMIT_FD),
            RestoreVar::new(ENV_PARENT_PID),
            RestoreVar::new(ENV_PARENT_BIRTH),
        ];
        let staged = stage_outgoing_handoff("ok", 3, None);
        let (ready_read, ready_write) = pipe_pair("ready");
        let (commit_read, commit_write) = pipe_pair("commit");
        staged.publish_env(
            ready_write,
            commit_read,
            Some(encode_target_identity(
                crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
                crate::build_info::GIT_COMMIT,
            )),
        );

        let (proof, ready, adopted) = child_proof().expect("the child adopts and proves");
        assert_eq!(adopted.len(), 3, "every session is adopted, none lost");
        assert_eq!(
            proof, staged.expected,
            "THE HANDOFF COMPLETES: the child's proof equals the parent's expectation"
        );
        assert!(ready.signal_proof(proof), "ProofReady is published");

        // The parent's side of the wire: read ProofReady, then Commit.
        let mut wire = [0u8; READY_WIRE_LEN];
        // SAFETY: bounded read into a live fixed buffer from our own pipe.
        let read = unsafe { libc::read(ready_read, wire.as_mut_ptr().cast(), wire.len()) };
        assert_eq!(
            read, READY_WIRE_LEN as isize,
            "the whole proof wire arrives"
        );
        assert_eq!(
            AdoptionProof::from_wire(&wire),
            Some(staged.expected),
            "the parent recognizes the proof it was waiting for"
        );
        assert!(
            staged
                .expected
                .commit_wire_matches(&staged.expected.to_commit_wire()),
            "the exact Commit the parent would send is the one the child accepts"
        );
        assert_ne!(
            staged.expected.to_commit_wire(),
            staged.expected.to_wire(),
            "Commit and ProofReady are distinct authorities on the wire"
        );

        for fd in [ready_read, commit_read, commit_write] {
            aterm_pty::close_fd(fd);
        }
        staged.teardown();
    }

    /// THE REGRESSION GUARD FOR FIX #1. The outgoing process is a DIFFERENT
    /// BUILD: its layout codec emitted `schema = 1`, which this binary's
    /// `RestoreManifest::from_toml` accepts and then rewrites to `schema = 2`.
    ///
    /// Hashing the WIRE (what the child does now) matches the parent exactly.
    /// Re-serializing the parse (what the child did until 2026-07) produces a
    /// different digest and therefore `AdoptionMismatch` — which is what every
    /// handoff in the field actually died of. The second assertion below IS the
    /// revert check: it computes the old child's digest and shows it differs.
    #[test]
    #[cfg(unix)]
    fn a_handoff_across_a_version_boundary_still_completes() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore = [
            RestoreVar::new("XDG_RUNTIME_DIR"),
            RestoreVar::new("HOME"),
            RestoreVar::new(ENV_MANIFEST),
            RestoreVar::new(ENV_NONCE),
            RestoreVar::new(ENV_FDS),
            RestoreVar::new(ENV_LAYOUT),
            RestoreVar::new(ENV_TARGET),
            RestoreVar::new(ENV_READY_FD),
            RestoreVar::new(ENV_COMMIT_FD),
            RestoreVar::new(ENV_PARENT_PID),
            RestoreVar::new(ENV_PARENT_BIRTH),
        ];
        let older_codec = |wire: &str| {
            assert!(wire.contains("schema = 2"), "today's wire: {wire}");
            wire.replacen("schema = 2", "schema = 1", 1)
        };
        let staged = stage_outgoing_handoff("xver", 1, Some(&older_codec));
        // What the OLD child would have committed to: re-serialize its parse.
        let old_wire = std::fs::read_to_string(staged.manifest_path.with_extension("layout.toml"))
            .expect("sidecar");
        let reserialized = crate::restore::RestoreManifest::from_toml(&old_wire)
            .expect("the child still ACCEPTS the older wire")
            .to_toml()
            .expect("reserialize");
        assert_ne!(
            reserialized, old_wire,
            "precondition: this binary's codec is NOT a fixed point on the older wire"
        );
        assert_ne!(
            layout_wire_digest(&reserialized),
            layout_wire_digest(&old_wire),
            "REVERTING fix #1 (re-serializing the parse) provably breaks the proof"
        );

        let (ready_read, ready_write) = pipe_pair("ready");
        let (commit_read, commit_write) = pipe_pair("commit");
        staged.publish_env(
            ready_write,
            commit_read,
            Some(encode_target_identity(
                crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
                crate::build_info::GIT_COMMIT,
            )),
        );
        let (proof, _ready, _adopted) = child_proof().expect("the child adopts and proves");
        assert_eq!(
            proof, staged.expected,
            "hashing the WIRE makes the two sides agree ACROSS the version boundary"
        );

        drop(_ready);
        for fd in [ready_read, commit_read, commit_write] {
            aterm_pty::close_fd(fd);
        }
        staged.teardown();
    }

    /// THE REGRESSION GUARD FOR THE SCREEN HALF OF FIX #1 — and it is a
    /// SEPARATE guard on purpose. The layout test above rewrites only the layout
    /// sidecar, so reverting the SCREEN commitment to the old
    /// re-derive-from-the-rebuilt-checkpoint behaviour left every test green: the
    /// screen half was covered vacuously. It no longer is.
    ///
    /// Here the outgoing process is a NEWER build whose `CheckpointMeta` carries
    /// one extra key. `CheckpointMeta` has no `deny_unknown_fields`, so this
    /// binary parses the carry happily and silently DROPS the key — and would
    /// then hash 17 fields where the parent hashed 18. Hashing the carried bytes
    /// makes the two sides agree anyway; re-deriving from the parse cannot.
    #[test]
    #[cfg(unix)]
    fn a_handoff_across_a_screen_meta_version_boundary_still_completes() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore = [
            RestoreVar::new("XDG_RUNTIME_DIR"),
            RestoreVar::new("HOME"),
            RestoreVar::new(ENV_MANIFEST),
            RestoreVar::new(ENV_NONCE),
            RestoreVar::new(ENV_FDS),
            RestoreVar::new(ENV_LAYOUT),
            RestoreVar::new(ENV_TARGET),
            RestoreVar::new(ENV_READY_FD),
            RestoreVar::new(ENV_COMMIT_FD),
            RestoreVar::new(ENV_PARENT_PID),
            RestoreVar::new(ENV_PARENT_BIRTH),
        ];
        // One additive key, exactly as a newer build's meta codec would emit it.
        let newer_meta_codec = |meta: &str| {
            assert!(meta.starts_with('{'), "meta json: {meta}");
            meta.replacen('{', "{\"scene_overlay\":true,", 1)
        };
        let staged = stage_outgoing_handoff_with("xvermeta", 2, None, Some(&newer_meta_codec));

        let (ready_read, ready_write) = pipe_pair("ready");
        let (commit_read, commit_write) = pipe_pair("commit");
        staged.publish_env(
            ready_write,
            commit_read,
            Some(encode_target_identity(
                crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
                crate::build_info::GIT_COMMIT,
            )),
        );
        let (proof, _ready, adopted) = child_proof().expect("the child adopts and proves");
        assert_eq!(
            adopted.len(),
            2,
            "both sessions adopt across the meta boundary"
        );
        assert_eq!(
            proof, staged.expected,
            "hashing the CARRIED META BYTES makes the two sides agree across a \
             CheckpointMeta version boundary; re-deriving from the parse does not"
        );

        drop(_ready);
        for fd in [ready_read, commit_read, commit_write] {
            aterm_pty::close_fd(fd);
        }
        staged.teardown();
    }

    /// FAIL CLOSED #1 — a WRONG BUILD cannot claim the PTYs. The parent
    /// authorized a different build; the child refuses before any proof exists,
    /// so no ready signal is ever published.
    #[test]
    #[cfg(unix)]
    fn a_wrong_build_child_refuses_the_handoff() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore = [
            RestoreVar::new("XDG_RUNTIME_DIR"),
            RestoreVar::new("HOME"),
            RestoreVar::new(ENV_MANIFEST),
            RestoreVar::new(ENV_NONCE),
            RestoreVar::new(ENV_FDS),
            RestoreVar::new(ENV_LAYOUT),
            RestoreVar::new(ENV_TARGET),
            RestoreVar::new(ENV_READY_FD),
            RestoreVar::new(ENV_COMMIT_FD),
            RestoreVar::new(ENV_PARENT_PID),
            RestoreVar::new(ENV_PARENT_BIRTH),
        ];
        let staged = stage_outgoing_handoff("wrongbuild", 1, None);
        let (ready_read, ready_write) = pipe_pair("ready");
        let (commit_read, commit_write) = pipe_pair("commit");
        let own = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
        staged.publish_env(
            ready_write,
            commit_read,
            Some(encode_target_identity(
                own.wrapping_add(1),
                crate::build_info::GIT_COMMIT,
            )),
        );
        assert!(
            child_proof().is_none(),
            "a candidate that is not the authorized build must never publish a proof"
        );
        for fd in [ready_read, ready_write, commit_read, commit_write] {
            aterm_pty::close_fd(fd);
        }
        staged.teardown();
    }

    /// FAIL CLOSED #2 — a WRONG SESSION SET cannot pass. The child's proof
    /// commits to the `(local_id, fd, pid)` set it ACTUALLY adopted, so a
    /// dropped or substituted session yields a digest the parent rejects.
    #[test]
    #[cfg(unix)]
    fn a_wrong_session_set_cannot_match_the_parent_expectation() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore = [
            RestoreVar::new("XDG_RUNTIME_DIR"),
            RestoreVar::new("HOME"),
            RestoreVar::new(ENV_MANIFEST),
            RestoreVar::new(ENV_NONCE),
            RestoreVar::new(ENV_FDS),
            RestoreVar::new(ENV_LAYOUT),
            RestoreVar::new(ENV_TARGET),
            RestoreVar::new(ENV_READY_FD),
            RestoreVar::new(ENV_COMMIT_FD),
            RestoreVar::new(ENV_PARENT_PID),
            RestoreVar::new(ENV_PARENT_BIRTH),
        ];
        let staged = stage_outgoing_handoff("wrongset", 3, None);
        let (ready_read, ready_write) = pipe_pair("ready");
        let (commit_read, commit_write) = pipe_pair("commit");
        staged.publish_env(
            ready_write,
            commit_read,
            Some(encode_target_identity(
                crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
                crate::build_info::GIT_COMMIT,
            )),
        );
        let (_proof, ready, adopted) = child_proof().expect("the child adopts and proves");

        let mut subset = adopted.clone();
        subset.pop();
        assert_ne!(
            ready.proof(&subset),
            Some(staged.expected),
            "a SUBSET of the authorized PTY set must not prove complete adoption"
        );
        let mut swapped = adopted.clone();
        swapped[0].2 = swapped[0].2.wrapping_add(1);
        assert_ne!(
            ready.proof(&swapped),
            Some(staged.expected),
            "a substituted shell pid must not prove the authorized adoption"
        );
        assert_eq!(
            ready.proof(&adopted),
            Some(staged.expected),
            "…while the EXACT set still does"
        );

        drop(ready);
        for fd in [ready_read, commit_read, commit_write] {
            aterm_pty::close_fd(fd);
        }
        staged.teardown();
    }

    /// FAIL CLOSED #3 — ProofReady is NOT Commit. A child holding a valid proof
    /// must not accept the proof wire, a foreign build's Commit, or a Commit for
    /// a different session set as release authority.
    #[test]
    #[cfg(unix)]
    fn a_missing_or_foreign_commit_never_releases_the_readers() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore = [
            RestoreVar::new("XDG_RUNTIME_DIR"),
            RestoreVar::new("HOME"),
            RestoreVar::new(ENV_MANIFEST),
            RestoreVar::new(ENV_NONCE),
            RestoreVar::new(ENV_FDS),
            RestoreVar::new(ENV_LAYOUT),
            RestoreVar::new(ENV_TARGET),
            RestoreVar::new(ENV_READY_FD),
            RestoreVar::new(ENV_COMMIT_FD),
            RestoreVar::new(ENV_PARENT_PID),
            RestoreVar::new(ENV_PARENT_BIRTH),
        ];
        let staged = stage_outgoing_handoff("nocommit", 2, None);
        let (ready_read, ready_write) = pipe_pair("ready");
        let (commit_read, commit_write) = pipe_pair("commit");
        staged.publish_env(
            ready_write,
            commit_read,
            Some(encode_target_identity(
                crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
                crate::build_info::GIT_COMMIT,
            )),
        );
        let (proof, _ready, adopted) = child_proof().expect("the child adopts and proves");

        assert!(
            !proof.commit_wire_matches(&proof.to_wire()),
            "the ProofReady wire is NOT a Commit: the magic differs"
        );
        let mut subset = adopted;
        subset.pop();
        let other = adoption_proof(
            &staged.nonce,
            crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
            crate::build_info::GIT_COMMIT,
            &[0x11; 32],
            &[0x22; 32],
            &subset,
        )
        .expect("a foreign proof");
        assert!(
            !proof.commit_wire_matches(&other.to_commit_wire()),
            "a Commit minted for a different attempt must not release these readers"
        );
        let mut corrupt = proof.to_commit_wire();
        corrupt[READY_WIRE_LEN - 1] ^= 0x01;
        assert!(
            !proof.commit_wire_matches(&corrupt),
            "a one-bit-corrupted Commit must not release these readers"
        );
        assert!(
            proof.commit_wire_matches(&proof.to_commit_wire()),
            "…only the exact Commit does"
        );

        drop(_ready);
        for fd in [ready_read, commit_read, commit_write] {
            aterm_pty::close_fd(fd);
        }
        staged.teardown();
    }

    /// B4's replacement proof term, measured instead of remembered.
    /// [`adoption_proof`] explains why the fd number stops agreeing the moment
    /// the masters travel over `SCM_RIGHTS`, and names the PTY's own device minor
    /// as what survives. Two facts have to hold for that to be a PROOF rather
    /// than a better-looking integer, and both were until now recorded as
    /// something somebody once saw on a terminal:
    ///
    /// * SUBSTITUTION. Simultaneously-open masters must answer DIFFERENT minors.
    ///   Without this the term detects a permutation and nothing else — which is
    ///   exactly the objection that disqualifies the transfer-order ordinal
    ///   sketched in `tests/handoff_launchd_job.rs`, and it would be no less
    ///   fatal here.
    /// * IDENTITY OF THE OPEN FILE DESCRIPTION. `dup` must preserve it, because
    ///   that is the same property `SCM_RIGHTS` leans on when it hands the
    ///   description itself to another process. The fd number is the thing that
    ///   changes across both; the PTY is not.
    ///
    /// macOS only, and that is the claim rather than a portability gap:
    /// `/dev/ptmx` is a cloning device there, so each open takes its own minor
    /// and `st_ino`/`st_dev` cannot tell two masters apart at all. Linux's
    /// masters share one `st_rdev` and expose the number through
    /// `ioctl(TIOCGPTN)` — a different measurement, for the lane that needs it to
    /// assert when it exists.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_replacement_proof_term_distinguishes_masters_and_survives_dup() {
        // Darwin's `minor(3)`: the low 24 bits of `st_rdev`. Masked rather than
        // cast, so the sign bit of a `dev_t` cannot ride into the comparison.
        fn pty_device_minor(fd: i32) -> i32 {
            // SAFETY: `fstat` only fills the zeroed `stat` out-parameter, and
            // `fd` is a live descriptor the caller holds across the whole call.
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let rc = unsafe { libc::fstat(fd, &mut st) };
            assert_eq!(rc, 0, "fstat on a live PTY master");
            st.st_rdev & 0x00ff_ffff
        }

        let (mut masters, mut slaves) = ([0i32; 3], [0i32; 3]);
        for (master, slave) in masters.iter_mut().zip(slaves.iter_mut()) {
            // SAFETY: valid out-params; openpty fills them on success.
            let rc = unsafe {
                libc::openpty(
                    master,
                    slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(rc, 0, "openpty");
        }
        let minors = [
            pty_device_minor(masters[0]),
            pty_device_minor(masters[1]),
            pty_device_minor(masters[2]),
        ];
        assert!(
            minors[0] != minors[1] && minors[1] != minors[2] && minors[0] != minors[2],
            "three simultaneously-open masters must answer three minors, got {minors:?}"
        );

        // AND THE MINOR IDENTIFIES THE PTY, which is the half that makes it a term
        // at all rather than a distinct counter: it is the number in the
        // `/dev/ttysNNN` the SLAVE answers to. The doc above claimed this from
        // recollection; measure it.
        for (i, &slave) in slaves.iter().enumerate() {
            let mut name = [0i8; 128];
            // SAFETY: `slave` is a live descriptor this test owns and `name` is a
            // 128-byte buffer whose length is passed alongside it.
            let rc = unsafe { libc::ttyname_r(slave, name.as_mut_ptr(), name.len()) };
            assert_eq!(rc, 0, "ttyname_r on a live PTY slave");
            // SAFETY: ttyname_r NUL-terminates on success.
            let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
                .to_str()
                .expect("a device path is ASCII");
            let n: i32 = path
                .strip_prefix("/dev/ttys")
                .expect("a Darwin PTY slave is /dev/ttysNNN")
                .parse()
                .expect("the NNN parses through its zero padding");
            assert_eq!(
                n, minors[i],
                "the master's device minor IS the slave's /dev/ttysNNN ({path})"
            );
        }

        // SAFETY: duplicating a live descriptor this test owns.
        let duplicate = unsafe { libc::dup(masters[0]) };
        assert!(duplicate >= 0, "dup");
        assert_ne!(
            duplicate, masters[0],
            "the duplicate is a different NUMBER for the same PTY"
        );
        assert_eq!(
            pty_device_minor(duplicate),
            minors[0],
            "the term follows the open file description, not the descriptor table slot"
        );

        aterm_pty::close_fd(duplicate);
        for (master, slave) in masters.into_iter().zip(slaves) {
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }
}

/// F4 ADJUDICATION — the adoption-proof asymmetry that WAS.
///
/// HISTORICAL, AND NOW A REGRESSION FENCE. Until 2026-07 the parent committed
/// to LIVE in-memory state while the child committed to the SAME state after a
/// serialize→deserialize→RE-SERIALIZE round trip performed by a DIFFERENT
/// BINARY. These tests characterize the codecs whose drift that arrangement
/// turned into `AdoptionMismatch`. The shipping child no longer re-serializes
/// anything — it hashes the exact bytes it consumed (`layout_wire_digest`,
/// `screen_wire_digest`) — so each "break" below is now a statement about the
/// CODEC, not about the protocol, and the protocol-level proof that the fix
/// holds lives in `tests::a_handoff_across_a_version_boundary_still_completes`.
#[cfg(test)]
mod f4_adoption_proof_asymmetry {
    use super::*;
    use crate::restore::{
        RestoreManifest, RestoredSplitTree, RestoredTab, RestoredView, TerminalLeafRestore,
        WindowLayout,
    };

    /// EXACTLY what `layout_digest` does, but keyed on the WIRE the parent
    /// wrote rather than on a struct. `layout_digest(m) == wire_digest(m.to_toml())`
    /// by construction (see `seamless::layout_digest`), so this lets a test stand in
    /// for "build A's parent-side digest" using only build A's bytes.
    fn wire_digest(wire: &str) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(LAYOUT_PROOF_DOMAIN);
        hasher.update(u64::try_from(wire.len()).unwrap().to_be_bytes());
        hasher.update(wire.as_bytes());
        hasher.finalize().into()
    }

    /// Exactly what the child USED TO do: re-derive the digest from the PARSED
    /// layout. Retained here as the counterfactual the fence measures against.
    fn child_digest_of_wire(wire: &str) -> Option<[u8; 32]> {
        layout_digest(&RestoreManifest::from_toml(wire)?)
    }

    fn realistic_layout() -> RestoreManifest {
        RestoreManifest::new(vec![WindowLayout {
            rows: 46,
            cols: 168,
            active_tab: 0,
            outer_x: Some(120),
            outer_y: Some(64),
            maximized: None,
            tabs: Vec::new(),
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: Some(0),
            restored_tabs: vec![RestoredTab {
                root: RestoredSplitTree::Split {
                    axis: crate::restore::SplitKind::Vertical,
                    ratio: 0.618_034,
                    first: Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
                        TerminalLeafRestore {
                            cwd: Some("/Users//example/aterm".to_string()),
                            title: "zsh".to_string(),
                            profile: None,
                            local_id: Some(0),
                            user_title: None,
                            description: None,
                            icon: None,
                            role: None,
                            attention: None,
                        },
                    ))),
                    second: Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
                        TerminalLeafRestore {
                            cwd: Some("/Users//example".to_string()),
                            title: "claude".to_string(),
                            profile: None,
                            local_id: Some(1),
                            user_title: None,
                            description: None,
                            icon: None,
                            role: None,
                            attention: None,
                        },
                    ))),
                },
                focused_path: vec![crate::restore::RestoreBranch::Second],
                zoomed: false,
            }],
        }])
    }

    /// CONTROL. Same binary on both ends: the parent's live digest and the
    /// child's reparsed digest agree. So F4 is NOT an unconditional break.
    #[test]
    fn same_build_parent_and_child_layout_digests_agree() {
        let layout = realistic_layout();
        let wire = layout.to_toml().expect("parent serializes its live layout");
        let parent = layout_digest(&layout).expect("parent digest");
        assert_eq!(
            parent,
            wire_digest(&wire),
            "parent digest == digest of wire"
        );
        let child = child_digest_of_wire(&wire).expect("child parses the parent wire");
        assert_eq!(
            parent, child,
            "same-build round trip is a fixed point:\n{wire}"
        );
    }

    /// CROSS-VERSION BREAK #1 — the manifest SCHEMA constant.
    /// `RestoreManifest::from_toml` ACCEPTS `LEGACY_SCHEMA` and then
    /// unconditionally REWRITES `manifest.schema = SCHEMA`. A parent built when
    /// `SCHEMA == 1` writes `schema = 1`; the child re-emits `schema = 2`. The
    /// adoption proof therefore CANNOT match, and nothing in the protocol
    /// notices — the parent reports `AdoptionMismatch`.
    #[test]
    fn cross_version_schema_bump_breaks_the_layout_digest() {
        let layout = realistic_layout();
        let modern = layout.to_toml().expect("serialize");
        assert!(modern.contains("schema = 2"), "today's wire: {modern}");
        // The bytes an older parent (SCHEMA == 1) would have written+hashed.
        let old_parent_wire = modern.replacen("schema = 2", "schema = 1", 1);
        let old_parent_digest = wire_digest(&old_parent_wire);
        let child_digest =
            child_digest_of_wire(&old_parent_wire).expect("child still ACCEPTS the legacy schema");
        assert_ne!(
            old_parent_digest, child_digest,
            "a schema bump must break the proof"
        );
        eprintln!(
            "F4/schema: parent={} child={} (child re-emitted `schema = 2`)",
            hex12(&old_parent_digest),
            hex12(&child_digest)
        );
    }

    /// CROSS-VERSION BREAK #2 — an ADDED serde field.
    /// `RestoredTab::zoomed` is `#[serde(default)]` with NO
    /// `skip_serializing_if`, so it is ALWAYS emitted. A parent built before
    /// `zoomed` existed writes a wire without it; the child defaults it and
    /// re-emits `zoomed = false`. Same semantic layout, different digest.
    /// EVERY future additive field of this shape re-breaks the handoff.
    #[test]
    fn cross_version_added_field_breaks_the_layout_digest() {
        let modern = realistic_layout().to_toml().expect("serialize");
        assert!(modern.contains("zoomed = false"), "wire: {modern}");
        let old_parent_wire = modern.replacen("zoomed = false\n", "", 1);
        let old_parent_digest = wire_digest(&old_parent_wire);
        let child_digest = child_digest_of_wire(&old_parent_wire)
            .expect("child parses the older wire (serde default)");
        assert_ne!(
            old_parent_digest, child_digest,
            "an additive always-emitted field must break the proof"
        );
        eprintln!(
            "F4/field: parent={} child={} (child re-emitted `zoomed = false`)",
            hex12(&old_parent_digest),
            hex12(&child_digest)
        );
    }

    /// The general lemma both breaks are instances of: the child's digest
    /// equals the parent's IFF the child's parse+reserialize is a byte fixed
    /// point on the parent's wire. Nothing in the protocol establishes that
    /// across a version boundary, and the parent's own pre-flight round-trip
    /// check (`app_input.rs` ~579) runs the PARENT's codec against itself.
    #[test]
    fn layout_digest_equality_is_exactly_wire_fixed_pointness() {
        let modern = realistic_layout().to_toml().expect("serialize");
        for (label, wire) in [
            ("identical build", modern.clone()),
            (
                "schema-1 parent",
                modern.replacen("schema = 2", "schema = 1", 1),
            ),
            (
                "pre-zoomed parent",
                modern.replacen("zoomed = false\n", "", 1),
            ),
        ] {
            let parsed = RestoreManifest::from_toml(&wire).expect("parses");
            let reserialized = parsed.to_toml().expect("reserialize");
            let fixed_point = reserialized == wire;
            let digests_match = child_digest_of_wire(&wire) == Some(wire_digest(&wire));
            assert_eq!(
                fixed_point, digests_match,
                "{label}: digest equality IS wire fixed-pointness"
            );
            eprintln!("F4/lemma: {label}: fixed_point={fixed_point} digests_match={digests_match}");
        }
    }

    // ---- screen side -------------------------------------------------------

    fn live_checkpoint() -> aterm_core::terminal::TerminalCheckpoint {
        let mut t = aterm_core::terminal::Terminal::new(24, 80);
        t.process(b"\x1b[1;38;5;202muser@mac\x1b[0m ~/aterm % cargo test");
        for i in 0..8 {
            t.process(format!("\r\ntest line {i}").as_bytes());
        }
        t.checkpoint_visible().expect("parser is Ground")
    }

    /// CONTROL for the screen side. The child never passed `screen_digest`
    /// through verbatim — it RE-DERIVED it from the reconstructed checkpoints,
    /// so the screen half carried the SAME round-trip exposure as the layout
    /// half. It now hashes the carried bytes instead. Same build ⇒ equal either
    /// way, which is what this control pins.
    #[test]
    fn same_build_parent_and_child_screen_digests_agree() {
        let cp = live_checkpoint();
        let parent = screen_digest(&[(0, cp.clone())]).expect("parent screen digest");

        // Exactly the child's path: meta JSON off the wire → CheckpointMeta →
        // into_checkpoint → screen_digest_refs.
        let carry = ScreenCarry {
            schema: ScreenCarry::SCHEMA,
            meta: serde_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta"),
            grid_file: "unused".to_string(),
            alt_grid_file: None,
        };
        let meta = parse_checkpoint_meta(&carry).expect("child parses meta");
        let rebuilt = meta.into_checkpoint(cp.grid.clone(), None);
        let child = screen_digest_refs(vec![(0, &rebuilt)]).expect("child screen digest");
        assert_eq!(
            parent, child,
            "same-build screen round trip is a fixed point"
        );
    }

    /// CROSS-VERSION BREAK #3 — the screen half, `CheckpointMeta` drift.
    /// `CheckpointMeta` has NO `deny_unknown_fields`. A parent built with an
    /// 18th meta field writes it; the older child silently DROPS it and hashes
    /// 17 keys. The parent hashed 18. `AdoptionMismatch`, with no diagnostic.
    #[test]
    fn cross_version_extra_meta_field_breaks_the_screen_digest() {
        let cp = live_checkpoint();
        let meta_json =
            serde_json::to_string(&CheckpointMeta::from_checkpoint(&cp)).expect("meta json");
        // What a NEWER parent's wire looks like: one additive key the running
        // (older) child does not know.
        let newer_parent_meta = meta_json.replacen('{', "{\"scene_overlay\":true,", 1);
        let parent_digest = {
            let mut hasher = Sha256::new();
            hasher.update(newer_parent_meta.as_bytes());
            let d: [u8; 32] = hasher.finalize().into();
            d
        };
        let carry = ScreenCarry {
            schema: ScreenCarry::SCHEMA,
            meta: newer_parent_meta.clone(),
            grid_file: "unused".to_string(),
            alt_grid_file: None,
        };
        let parsed = parse_checkpoint_meta(&carry).expect("child parses (no deny_unknown)");
        let child_meta = serde_json::to_string(&parsed).expect("child reserializes");
        assert_ne!(
            newer_parent_meta, child_meta,
            "the child dropped the unknown key"
        );
        let child_digest = {
            let mut hasher = Sha256::new();
            hasher.update(child_meta.as_bytes());
            let d: [u8; 32] = hasher.finalize().into();
            d
        };
        assert_ne!(parent_digest, child_digest);
        eprintln!(
            "F4/meta: parent meta {} bytes, child meta {} bytes — screen_digest inputs differ",
            newer_parent_meta.len(),
            child_meta.len()
        );
    }

    /// CROSS-VERSION BREAK #4 — the HARDER screen failure. `parse_checkpoint_meta`
    /// demands EVERY key in its `REQUIRED` list. If a newer child adds an 18th
    /// meta field to that list, an older parent's 17-key wire is REFUSED
    /// outright: the child adopts NOTHING, never writes a proof, and the parked
    /// parent reads EOF — `ChildDied`, the 0.58-era symptom.
    #[test]
    fn cross_version_missing_required_meta_key_refuses_adoption_entirely() {
        let cp = live_checkpoint();
        let mut value: serde_json::Value =
            serde_json::to_value(CheckpointMeta::from_checkpoint(&cp)).expect("meta value");
        value
            .as_object_mut()
            .expect("object")
            .remove("secure_keyboard_entry")
            .expect("key was present");
        let carry = ScreenCarry {
            schema: ScreenCarry::SCHEMA,
            meta: serde_json::to_string(&value).expect("json"),
            grid_file: "unused".to_string(),
            alt_grid_file: None,
        };
        assert!(
            parse_checkpoint_meta(&carry).is_none(),
            "a single missing REQUIRED key destroys the whole adoption"
        );
    }

    // ---- build / commit identity -------------------------------------------

    /// F4(b). The child proves with ITS OWN `build_info::BUILD_NUMBER` /
    /// `GIT_COMMIT`; the parent expects the apply ticket's
    /// `target_build`/`target_commit`. Any disagreement is a silent
    /// `AdoptionMismatch`.
    #[test]
    fn child_build_and_commit_must_equal_the_parents_expectation_exactly() {
        let ids = [(0u64, 7i32, 4242i32)];
        let nonce = "0123456789abcdef0123456789abcdef";
        let (ld, sd) = ([1u8; 32], [2u8; 32]);
        let base = adoption_proof(nonce, 1_784_869_524, "7bf23bbe1234", &ld, &sd, &ids).unwrap();

        // (i) the swap did not take: the child is still the OLD binary.
        let stale_build =
            adoption_proof(nonce, 1_784_825_157, "7bf23bbe1234", &ld, &sd, &ids).unwrap();
        assert_ne!(base, stale_build, "build number mismatch breaks the proof");

        // (ii) a `-dirty` working tree on either side.
        let dirty =
            adoption_proof(nonce, 1_784_869_524, "7bf23bbe1234-dirty", &ld, &sd, &ids).unwrap();
        assert_ne!(base, dirty, "`-dirty` breaks the proof");

        // (iii) SAFE: 40-hex vs 12-hex vs mixed case all normalize together.
        let long = adoption_proof(
            nonce,
            1_784_869_524,
            "7BF23BBE1234567890ABCDEF1234567890ABCDEF",
            &ld,
            &sd,
            &ids,
        )
        .unwrap();
        assert_eq!(base, long, "commit length/case normalization is symmetric");

        // (iv) an un-stamped build cannot prove at all in a RELEASE binary:
        // `unknown` is admitted ONLY under debug_assertions or the debug env.
        let unknown = adoption_proof(nonce, 1_784_869_524, "unknown", &ld, &sd, &ids);
        let admitted =
            cfg!(debug_assertions) || std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some();
        assert_eq!(
            unknown.is_some(),
            admitted,
            "`unknown` commit is release-fatal (no proof is ever emitted)"
        );
    }

    /// THE TRIPWIRE for the frozen cross-version wire, and the only test in this
    /// file that does not compare [`adoption_proof`] against itself. Every other
    /// one stays green while a changed domain string, a reordered field, a
    /// dropped length prefix or a different [`normalize_commit`] quietly makes
    /// the outgoing build and the incoming build compute different digests — and
    /// that disagreement is `AdoptionMismatch`, which `finish_update_handoff`
    /// latches manual-only for every parent already in the field. A self-
    /// consistent suite cannot see it, because both sides of every comparison
    /// move together.
    ///
    /// So this compares against a constant derived once from the wire as
    /// specified, and the input is chosen to hold down the parts that are easiest
    /// to change by accident: an unsorted identity set (the sort is hashed
    /// membership, not presentation) and a 40-hex uppercase commit (the
    /// fold-and-truncate to 12 is hashed too).
    ///
    /// If this fails, the question is never "what is the digest now". It is
    /// whether the edit was meant to be a wire break at all — and if it was,
    /// nothing in the protocol tells the parents in the field why their update
    /// lane stopped working.
    #[test]
    fn the_v1_adoption_digest_is_frozen_to_this_exact_vector() {
        const FROZEN: &str = "9f32e4a273bbb14fb95b80b90788b41fce5cbfbc9fff94051934eb9b7d78ccbd";

        let ids = [(2u64, 9i32, 4242i32), (0, 7, 101), (1, 8, 777)];
        let proof = adoption_proof(
            "0123456789abcdef0123456789abcdef",
            1_784_869_524,
            "7BF23BBE1234567890ABCDEF1234567890ABCDEF",
            &[1u8; 32],
            &[2u8; 32],
            &ids,
        )
        .expect("a well-formed identity set proves");
        assert_eq!(proof.count, 3, "the count rides beside the digest");
        let digest: String = proof.digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            digest, FROZEN,
            "the LIVE adoption wire changed — read this test's doc before touching the constant"
        );
    }

    fn hex12(d: &[u8; 32]) -> String {
        d[..6].iter().map(|b| format!("{b:02x}")).collect()
    }
}
