// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live seamless-update handoff.

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
#[cfg(unix)]
const MAX_HANDOFF_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INCOMING_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
#[cfg(any(unix, test))]
const MAX_HANDOFF_AGGREGATE_META_BYTES: u64 = 3 * 1024 * 1024;
/// The largest layout sidecar a successor reads; `handoff_diet`'s tests read one the same way.
pub(crate) const MAX_HANDOFF_LAYOUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_HANDOFF_GRID_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HANDOFF_AGGREGATE_GRID_BYTES: u64 = 256 * 1024 * 1024;
const MAX_HANDOFF_GRID_CELLS: u64 = MAX_HANDOFF_AGGREGATE_GRID_CELLS / 2;

#[cfg(any(unix, test))]
const LEGACY_HANDOFF_GRID_CELLS: u64 = 32 * 1024;

#[cfg(any(unix, test))]
const RELAXED_CONSUMER_FROM_BUILD: u64 = 1_790_305_290;
const MAX_HANDOFF_HISTORY_LINES: u32 = 256;

/// The producer's per-session history target: the wire ceiling, so a healthy capture carries all the protocol allows.
#[must_use]
#[cfg(unix)]
pub(crate) const fn max_handoff_history_lines() -> u32 {
    MAX_HANDOFF_HISTORY_LINES
}

const MAX_HANDOFF_AGGREGATE_GRID_CELLS: u64 = 4 * 1024 * 1024;
#[cfg(unix)]
const READY_WIRE_MAGIC: &[u8; 4] = b"ASR1";
#[cfg(unix)]
const COMMIT_WIRE_MAGIC: &[u8; 4] = b"ASC1";
#[cfg(unix)]
pub(crate) const READY_WIRE_LEN: usize = 4 + 4 + 32;

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
const fn handoff_is_modern_overlap(present: HandoffEnvPresence) -> bool {
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
    let out_of_band = cfg!(target_os = "macos") && rendezvous && claim && !fds && !ready && !commit;
    inherited || out_of_band
}

/// One adopted session's identity on the wire: pool-local session id, inherited PTY master fd and shell pid, in that order.
pub(crate) type SessionIdentity = (u64, i32, i32);

/// Exact, attempt-bound proof of the PTYs the incoming process really adopted.
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

/// Canonical SHA-256 commitment to one complete adopted session set: the attempt nonce, the authorized target build/commit, the layout and screen digests, and the sorted [`SessionIdentity`] triples.
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

/// Why [`adoption_proof`] formed no proof: which input the frozen wire cannot carry, each said by its own cause.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(unix, test))]
pub(crate) enum ProofRefusal {
    /// More handed sessions than the wire's `u32` count holds.
    Sessions(usize),
    /// A nonce longer than the wire's `u32` length prefix holds.
    Nonce(usize),
    /// A commit that is no build stamp [`normalize_commit`] admits: in a release build, the `unknown` of a build made without its git facts.
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

/// [`normalize_commit`] with its refusal named: the spelling the adoption proof hashes, or [`ProofRefusal::UnstampedCommit`].
#[cfg(any(unix, test))]
pub(crate) fn handoff_commit_stamp(commit: &str) -> Result<String, ProofRefusal> {
    normalize_commit(commit).ok_or_else(|| ProofRefusal::UnstampedCommit(commit.trim().to_string()))
}

/// The one canonical spelling of a build's git commit, shared by every side of the handoff.
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
    (base == "unknown" && !dirty && cfg!(debug_assertions)).then(|| "unknown".to_string())
}

/// Canonical, attempt-bound commitment to the complete window/tab/pane layout.
#[must_use]
#[cfg(any(unix, test))]
pub(crate) fn layout_digest(layout: &crate::restore::RestoreManifest) -> Option<[u8; 32]> {
    layout_wire_digest(&layout.to_toml().ok()?)
}

/// [`layout_digest`] keyed on the exact serialized bytes.
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
    /// The inactive grid (`alt_grid`): the saved primary while the alternate screen is up.
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

/// Why a visible-checkpoint set could not be committed canonically.
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
        /// The first bound the meta broke, named.
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
    /// The screen metas cost the manifest more than [`MAX_HANDOFF_AGGREGATE_META_BYTES`] even with every carried shell mark shed ([`settle_wire_carries`]).
    MetasOverBudget {
        cost: u64,
        budget: u64,
    },
}

#[cfg(any(unix, test))]
impl ScreenDigestRefusal {
    /// The session this refusal blames, which [`settle_wire_carries`] lowers; `None` for the pool-wide arms (a duplicate id names an id, but as an identity fact).
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) const fn local_id(&self) -> Option<u64> {
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

/// The `cap`-independent half of what [`screen_digest_and_meta_cost`] demands of one checkpoint: the parser was Ground, the inactive grid and its cursor agree, and both grid blobs hold exactly the record count the wire declares and re-encode to the same bytes.
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

/// Canonical commitment to every required visible-screen checkpoint: sorted by local id, length framed, over canonical wire encodings only (non-canonical ones are refused before hashing).
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

#[cfg(test)]
fn screen_digest_refs(
    screens: Vec<(u64, &TerminalCheckpoint)>,
) -> Result<[u8; 32], ScreenDigestRefusal> {
    screen_digest_and_meta_cost(screens).map(|(digest, _)| digest)
}

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

struct ScreenWireEntry<'a> {
    local_id: u64,
    meta: &'a [u8],
    grid: &'a [u8],
    alt_grid: Option<&'a [u8]>,
}

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

#[cfg(unix)]
fn random_nonce() -> String {
    aterm_uds::rand::hex_token::<16>().unwrap_or_else(|_| "0".repeat(32))
}

/// Mint the attempt nonce an outgoing handoff is named by, before anything is written: the successor is launched with the manifest's path in its environment before the manifest exists, so the writer (`write_outgoing`) and the launch environment derive the same name from the same nonce ([`outgoing_manifest_path`]).
#[cfg(unix)]
pub(crate) fn mint_outgoing_nonce() -> String {
    random_nonce()
}

#[cfg(unix)]
fn outgoing_manifest_name(nonce: &str) -> String {
    format!("seamless-{}-{nonce}.toml", std::process::id())
}

/// Where `write_outgoing` will put the manifest for `nonce` — `None` when there is no private control dir (then no seamless handoff is possible at all).
#[cfg(unix)]
pub(crate) fn outgoing_manifest_path(nonce: &str) -> Option<std::path::PathBuf> {
    Some(crate::control_auth::socket_dir()?.join(outgoing_manifest_name(nonce)))
}

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

/// Why `write_outgoing` wrote nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(unix)]
pub(crate) enum WriteOutgoingFailure {
    /// No private control directory to write into.
    NoPrivateDir,
    /// The capture handed to the writer disagrees with itself (the session, screen and descriptor sets, a screen past a bound, a meta that will not serialize) — this build's own inconsistency.
    Inconsistent,
    /// The filesystem refused a write.
    Io(std::io::ErrorKind),
    /// The manifest would be past the cap the successor reads it under ([`MAX_HANDOFF_MANIFEST_BYTES`]) with every held pane shed: written, it would be refused whole, so it is not written.
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

/// Path to the person-stamps sidecar beside `manifest`.
#[cfg(unix)]
pub(crate) fn person_stamps_path(manifest: &std::path::Path) -> std::path::PathBuf {
    manifest.with_extension("person")
}

#[cfg(unix)]
const MAX_PERSON_STAMPS_BYTES: u64 = 64 * 1024;

/// One carried session's row in the person-stamps sidecar ([`write_person_stamps`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonStamp {
    /// A person last gave the session input this many milliseconds ago.
    Keyed(u64),
    /// No person ever did in the milliseconds the outgoing process (and those before it) served it (`human_input::HumanInputStamp::unkeyed_ms`).
    Unkeyed(u64),
}

/// Write the person-stamps sidecar at `path`.
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

/// Consume the person-stamps sidecar at `path` under `dir`.
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

/// Read a regular file without following a symlink, without blocking on whatever else sits at the path, and without allocating past its protocol cap.
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

/// [`take_regular_capped`]'s read without the consume.
pub(crate) fn read_regular_capped(path: &std::path::Path, max_bytes: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
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

/// The first bound a visible-checkpoint meta broke, named field by field: which value, which bound, by how much.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MetaBoundViolation {
    pub(crate) slot: &'static str,
    pub(crate) field: &'static str,
    pub(crate) value: u64,
    pub(crate) rule: BoundRule,
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

#[cfg(any(unix, test))]
fn checkpoint_meta_is_bounded(meta: &CheckpointMeta) -> bool {
    checkpoint_meta_bound_violation(meta).is_none()
}

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
    let rows = u64::from(rows).checked_add(u64::from(history))?;
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

/// Which ceiling [`admit_checkpoint_dimensions`] refused a geometry on, so a refusal names the cap that actually bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmitRefusal {
    /// The geometry or history is outside the protocol itself: a zero side, a side past the engine's `MAX_GRID_ROWS`/`MAX_GRID_COLS`, or more than `MAX_HANDOFF_HISTORY_LINES` carried lines.
    Dimension { rows: u16, cols: u16, history: u32 },
    /// One grid's `rows * cols` is over the per-grid cell ceiling in force.
    PerGrid { cells: u64, cap: u64 },
    /// This session's cost would take the handoff's decoded cells past the aggregate.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireCaps {
    per_grid_cells: u64,
    attrs_bounded_by_columns: bool,
    carries_parser: bool,
    carries_output_state: bool,
}

impl WireCaps {
    /// This build's own ceilings — what its consumer and `screen_digest` enforce.
    #[must_use]
    pub(crate) const fn current() -> Self {
        Self {
            per_grid_cells: MAX_HANDOFF_GRID_CELLS,
            attrs_bounded_by_columns: false,
            carries_parser: true,
            carries_output_state: true,
        }
    }

    /// The frozen ceilings of every consumer before [`RELAXED_CONSUMER_FROM_BUILD`] (audited against v0.91.0; v0.92.0 enforces the same).
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) const fn legacy() -> Self {
        Self {
            per_grid_cells: LEGACY_HANDOFF_GRID_CELLS,
            attrs_bounded_by_columns: true,
            carries_parser: false,
            carries_output_state: false,
        }
    }

    /// The ceilings of the successor at `target_build`, handed to by the running build.
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) fn for_target(target_build: u64) -> Self {
        Self::for_hop(crate::running_build_number(), target_build)
    }

    /// [`Self::for_target`] with the running build named, so a test can pin both directions of a hop.
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) const fn for_hop(running_build: u64, target_build: u64) -> Self {
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

    /// Whether the successor continues a partial escape sequence the carry holds.
    #[cfg(unix)]
    #[must_use]
    pub(crate) const fn carries_parser(self) -> bool {
        self.carries_parser
    }

    /// Whether the successor resumes with the engine's in-flight output state the carry holds (`CheckpointMeta.output_state`).
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

    /// [`admit_checkpoint_dimensions`] against these ceilings: the producer's seam, which must price every carry at the target's cap.
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
        let cells = u64::from(rows)
            .checked_mul(u64::from(cols))
            .ok_or(dimension)?;
        if cells > self.per_grid_cells {
            return Err(AdmitRefusal::PerGrid {
                cells,
                cap: self.per_grid_cells,
            });
        }
        let history_cells = u64::from(history)
            .checked_mul(u64::from(cols))
            .ok_or(dimension)?;
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

/// One dimension/allocation admission seam shared by UI pre-capture, outgoing digest/write, and incoming decode, at this build's own ceilings ([`WireCaps::current`]).
pub(crate) fn admit_checkpoint_dimensions(
    used_cells: &mut u64,
    rows: u16,
    cols: u16,
    history: u32,
    has_alt: bool,
) -> Result<u64, AdmitRefusal> {
    WireCaps::current().admit(used_cells, rows, cols, history, has_alt)
}

/// [`admit_checkpoint_dimensions`] for a crash restore's checkpoint (`crash_restore`).
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

/// What one session's mandatory carry costs the aggregate: its visible grid plus the alt grid the producer conservatively reserves, exactly what `admit_checkpoint_dimensions(used, rows, cols, 0, true)` charges.
#[must_use]
#[cfg(unix)]
pub(crate) fn mandatory_checkpoint_cells(rows: u16, cols: u16) -> u64 {
    u64::from(rows)
        .saturating_mul(u64::from(cols))
        .saturating_mul(2)
}

/// The same pricing for the producer's capture-byte budget: twice `dimension_grid_cap`, what the capture loop charges per admitted session (the alt grid is reserved conservatively, as at pre-capture admission).
#[must_use]
#[cfg(any(unix, test))]
pub(crate) fn checkpoint_capture_budget_bytes(rows: u16, cols: u16, history: u32) -> Option<u64> {
    dimension_grid_cap(rows, cols, history)?.checked_mul(2)
}

/// The process-wide aggregate cell ceiling, so the producer prices a whole pool against the number `admit_checkpoint_dimensions` enforces.
#[must_use]
#[cfg(unix)]
pub(crate) const fn max_handoff_aggregate_grid_cells() -> u64 {
    MAX_HANDOFF_AGGREGATE_GRID_CELLS
}

fn line_record_cap(cols: u16) -> usize {
    16usize
        .saturating_mul(1024)
        .saturating_add(usize::from(cols).saturating_mul(512))
}

fn strict_grid_lines(
    bytes: &[u8],
    rows: u16,
    cols: u16,
    history: u32,
) -> Option<Vec<aterm_core::scrollback::Line>> {
    let content_cap = usize::from(cols).saturating_mul(256);
    let record_cap = line_record_cap(cols);
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

struct ScreenWireBytes {
    grid: Vec<u8>,
    alt_grid: Option<Vec<u8>>,
}

enum IncomingScreen {
    Exact(TerminalCheckpoint),
    Degraded {
        checkpoint: Option<TerminalCheckpoint>,
        cause: String,
        wire: ScreenWireBytes,
    },
}

struct IncomingSession {
    adopted: Adopted,
    degraded: Option<(String, ScreenWireBytes)>,
}

fn refuse_incoming_session<T>(local_id: u64, why: &str) -> Option<T> {
    aterm_log::warn!(
        "overlap handoff: session {local_id} {why}; refusing the whole handoff so the outgoing \
         process keeps every session"
    );
    None
}

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

fn admit_incoming_screen(
    meta: Option<CheckpointMeta>,
    wire: ScreenWireBytes,
    used_cells: &mut u64,
) -> IncomingScreen {
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

fn repaint_checkpoint(
    meta: Option<&CheckpointMeta>,
    used_cells: &mut u64,
) -> Option<TerminalCheckpoint> {
    repaint_checkpoint_within(meta, used_cells, WireCaps::current())
}

fn repaint_checkpoint_within(
    meta: Option<&CheckpointMeta>,
    used_cells: &mut u64,
    caps: WireCaps,
) -> Option<TerminalCheckpoint> {
    let meta = meta?;
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
    if checkpoint_meta_bound_violation(&sanitized).is_some() {
        return Some(keep_carried_title(
            keep_shell_integration_authority(blank, meta),
            meta,
        ));
    }
    let alt_grid = has_alt.then(|| blank.grid.clone());
    Some(sanitized.into_checkpoint(blank.grid, alt_grid))
}

/// Where on the producer's degrade ladder one session's screen was carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg(any(unix, test))]
pub(crate) enum CarryRung {
    /// The screen as the capture asked for it: the visible grid, the saved primary, and the scrollback depth the time and budget ladder chose.
    Full,
    /// A predicate refused the carried scrollback, so the screen went without it: the visible grid and every scalar exactly.
    VisibleOnly,
    /// A line record past the wire's per-record byte cap refused the exact screen, so it went with the hyperlinks of exactly those lines dropped ([`strip_over_cap_links`]): every character, attribute, underline colour, wrap flag and scalar is exact, and the scrollback is kept when it fits.
    StrippedLinks,
    /// The screen's grids exactly, with the scalar fields that broke a meta bound clamped ([`sanitize_checkpoint_for_wire`]).
    Sanitized,
    /// A blank, canonical screen carrying the sanitized scalar state: modes, keyboard protocols, charsets and the pen survive, the content redraws.
    Repaint,
}

#[cfg(any(unix, test))]
impl CarryRung {
    /// The successor must make the program redraw (`ScreenCarry::repaint`): its screen is not exactly what the program last drew.
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) const fn needs_repaint(self) -> bool {
        matches!(self, Self::Sanitized | Self::Repaint)
    }

    /// Whether the control carry's differ state (`handoff_carry::capture_head`) travels: only while the carried screen is the one it describes, since it diffs the program's next frame against that screen.
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

/// Clamp every scalar field of `checkpoint` that breaks one of this build's meta bounds, one field at a time in the consumer's order; the grids are never touched.
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

#[cfg(any(unix, test))]
fn log_degraded_carry(local_id: u64, rung: CarryRung, cause: &str) {
    if rung != CarryRung::Full {
        aterm_log::warn!("update apply: session {local_id} carried degraded ({rung}): {cause}");
    }
}

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

fn keep_carried_title(mut blank: TerminalCheckpoint, meta: &CheckpointMeta) -> TerminalCheckpoint {
    blank.title.clone_from(&meta.title);
    blank
}

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

#[cfg(any(unix, test))]
fn repaint_carry(
    meta: &CheckpointMeta,
    aggregate_cells: &mut u64,
    caps: WireCaps,
) -> TerminalCheckpoint {
    repaint_checkpoint_within(Some(meta), aggregate_cells, caps)
        .unwrap_or_else(|| unadmitted_blank_carry(meta))
}

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

#[cfg(any(unix, test))]
fn strip_over_cap_links(checkpoint: &mut TerminalCheckpoint) -> Option<usize> {
    strip_affordable(checkpoint).ok()?;
    let record_cap = line_record_cap(checkpoint.cols);
    let rows = usize::from(checkpoint.rows);
    let main = usize::try_from(checkpoint.history_lines)
        .ok()
        .and_then(|history| rows.checked_add(history))
        .and_then(|records| strip_blob_links(&checkpoint.grid, records, record_cap));
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

const MAX_LINK_STRIP_BYTES: u64 = 4 * 1024 * 1024;

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

/// The producer's ladder for one session: the most faithful carry of `terminal` this build's wire predicates admit at the successor's `caps`, its rung, and why it is below Full.
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
            None => {
                projection = Some(checkpoint);
                break;
            }
        }
    }
    let meta = projection.as_ref().map_or_else(
        || terminal.carry_meta_abandoning_partial(),
        CheckpointMeta::from_checkpoint,
    );
    let checkpoint = repaint_carry(&meta, aggregate_cells, caps);
    let cause = causes.join("; ");
    log_degraded_carry(local_id, CarryRung::Repaint, &cause);
    (checkpoint, CarryRung::Repaint, Some(cause))
}

/// The Repaint rung taken directly: for a session the capture loop's own decode-authority budget (`MAX_HANDOFF_CAPTURE_BUDGET_BYTES`, priced only by the producer) cannot afford at a higher rung, and for every session under a successor's `carry = "repaint"` policy.
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

/// [`carry_for_wire`] under the ceiling the successor's signed handoff policy sets ([`aterm_update_core::handoff_policy`]): the lower of the ladder's rung and the policy's.
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

/// One session's screen as the producer will commit it, and the rung it was carried at — the unit [`settle_wire_carries`] lowers.
#[derive(Debug, Clone)]
#[cfg(any(unix, test))]
pub(crate) struct WireCarry {
    pub(crate) local_id: u64,
    pub(crate) checkpoint: TerminalCheckpoint,
    pub(crate) rung: CarryRung,
}

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

#[cfg(any(unix, test))]
fn wire_bytes(checkpoint: &TerminalCheckpoint) -> u64 {
    let len = |blob: &[u8]| u64::try_from(blob.len()).unwrap_or(u64::MAX);
    len(&checkpoint.grid).saturating_add(checkpoint.alt_grid.as_deref().map_or(0, len))
}

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

#[cfg(any(unix, test))]
fn is_lowerable(carry: &WireCarry) -> bool {
    carry.rung != CarryRung::Repaint || (carry.checkpoint.rows, carry.checkpoint.cols) != (1, 1)
}

#[cfg(any(unix, test))]
fn largest_lowerable(carries: &[WireCarry], cost: fn(&TerminalCheckpoint) -> u64) -> Option<usize> {
    carries
        .iter()
        .enumerate()
        .filter(|(_, carry)| is_lowerable(carry))
        .max_by_key(|(_, carry)| cost(&carry.checkpoint))
        .map(|(index, _)| index)
}

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

/// The producer's self-check: commit the pool with `screen_digest` (this build's full predicate, the one its consumer runs) and where it refuses, lower the carry it blames and commit again instead of refusing the update.
#[cfg(any(unix, test))]
pub(crate) fn settle_wire_carries(
    carries: &mut [WireCarry],
    caps: WireCaps,
) -> Result<[u8; 32], ScreenDigestRefusal> {
    let mut lowerings_left = carries.len().saturating_mul(5);
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
            Ok((digest, _)) => match shed_metas_to_budget(carries) {
                MetaBudget::Fits => return Ok(digest),
                MetaBudget::Shed if meta_passes_left > 0 => {
                    meta_passes_left -= 1;
                    continue;
                }
                MetaBudget::Shed => return Ok(digest),
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

#[cfg(any(unix, test))]
fn meta_manifest_cost(checkpoint: &TerminalCheckpoint) -> u64 {
    aterm_json::to_vec(&CheckpointMeta::from_checkpoint(checkpoint))
        .map_or(u64::MAX, |json| meta_json_manifest_cost(&json))
}

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

#[cfg(any(unix, test))]
enum MetaBudget {
    Fits,
    Shed,
    Over { cost: u64 },
}

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

#[cfg(any(unix, test))]
fn meta_stacks_pushed(checkpoint: &TerminalCheckpoint) -> bool {
    checkpoint.color.has_stack()
        || checkpoint
            .title
            .as_ref()
            .is_some_and(|title| !title.stack.is_empty())
}

#[cfg(any(unix, test))]
fn meta_tab_stops_past_width(checkpoint: &TerminalCheckpoint) -> bool {
    std::iter::once(&checkpoint.cursor)
        .chain(&checkpoint.alt_cursor)
        .any(|cursor| cursor.tab_stops.len() > usize::from(checkpoint.cols))
}

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
const MAX_HELD_META_BYTES: usize = 64 * 1024;
/// The most grid bytes all held panes together may put on disk, and that a successor reads for them — on top of the handed sessions', inside the same aggregate caps, and always after them.
pub(crate) const MAX_HELD_AGGREGATE_GRID_BYTES: u64 = 16 * 1024 * 1024;

/// One held pane as the park captured it: the pool id its layout leaf names, its session id, its visible screen, and how its command ended when the outgoing process had collected that.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub(crate) struct HeldScreen {
    pub local_id: u64,
    pub sid: SessionId,
    pub checkpoint: TerminalCheckpoint,
    pub exit: Option<aterm_pty::ChildExit>,
    /// Its scrollback lines: what is left behind when no sidecar carries them.
    pub history_dropped: u64,
    /// The fence of its scrollback at the capture ([`crate::handoff_history::capture_head`]'s), which the held carry's join reads its export against ([`crate::handoff_history::name_held_sidecar`]); `None` carries none.
    pub history_head: Option<aterm_core::grid::HistoryFence>,
    /// Whether the person had seen the failure its exit marks the tab for ([`crate::session_status::StatusObserver::failure_seen`]).
    pub failure_seen: bool,
}

#[cfg(unix)]
impl HeldScreen {
    /// The grid bytes this screen puts on disk — what [`MAX_HELD_AGGREGATE_GRID_BYTES`] prices.
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
const HELD_GRID_SUFFIX: &str = ".heldgrid";
const HELD_ALT_SUFFIX: &str = ".heldalt";

fn held_history_name(stem: &str, local_id: u64) -> String {
    format!("{stem}.h{local_id}.hist")
}

fn held_control_name(stem: &str, local_id: u64) -> String {
    format!("{stem}.h{local_id}.ctl")
}

fn held_sidecar_name(stem: &str, local_id: u64, alt: bool) -> String {
    let suffix = if alt {
        HELD_ALT_SUFFIX
    } else {
        HELD_GRID_SUFFIX
    };
    format!("{stem}.h{local_id}{suffix}")
}

/// A held pane's screen meta as the manifest carries it, or why the writer will not carry the screen whatever the budgets say: a grid past the reader's link-strip bound ([`MAX_LINK_STRIP_BYTES`]) or a meta past [`MAX_HELD_META_BYTES`].
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

/// One held pane's final screen as a successor admitted it: what `App::restore_held_leaf` shows, read-only, in the pane its layout leaf names.
pub(crate) struct HeldPane {
    pub local_id: u64,
    /// The session id the outgoing process knew the pane by ([`crate::session_store::HeldPaneCarry::sid`]), when this process may take it, so `aterm ctl @<sid>` still reaches the pane.
    pub sid: Option<SessionId>,
    pub checkpoint: TerminalCheckpoint,
    /// How its command ended, as the outgoing process carried it ([`crate::session_store::HeldPaneCarry::exit_code`]); `None` when it carried neither, or (malformed) both.
    pub exit: Option<aterm_pty::ChildExit>,
    /// Its scrollback: the sidecar its outgoing process named, opened before the proof, and the lines that do not cross, counted; imported after Commit (`App::restore_held_leaf`).
    pub history: crate::handoff_history::AdoptedHistory,
    /// Its turn ledger and archive tail, read before the proof; `None` when the outgoing process carried none, and the pane's ledger then names the turn ids it cannot vouch for (`handoff_carry::adopted_ledger`).
    pub control: Option<crate::handoff_carry::ControlCarry>,
    /// Whether the person had seen the failure its exit marks the tab for ([`crate::session_store::HeldPaneCarry::failure_seen`]); `false` from an older producer.
    pub failure_seen: bool,
}

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
        let read = before.saturating_sub(budget);
        *remaining_bytes = remaining_bytes.saturating_sub(read);
        held_budget = held_budget.saturating_sub(read);
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
        checkpoint.output_state.program_state = None;
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
                        control.without_differ();
                        control
                    })
            }),
            failure_seen: carry.failure_seen,
        });
    }
    panes
}

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

/// The physical artifacts one outgoing attempt published, plus the one screen commitment computed over the exact bytes it wrote.
#[cfg(unix)]
pub(crate) struct OutgoingHandoff {
    pub manifest_path: String,
    pub nonce: String,
    pub fds_wire: String,
    /// [`screen_wire_digest`] over the carried meta/grid bytes as written.
    pub screen_digest: [u8; 32],
    /// The carried connections this manifest left out to fit its cap ([`shed_connections_to_fit`]).
    pub connections_left_out: Vec<ConnectionCarry>,
}

/// Write the outgoing handoff manifest (nonce-stamped, `0600`, in the `0700` control dir) and return the [`OutgoingHandoff`] whose first three fields become the child's env, or [`WriteOutgoingFailure::NoPrivateDir`] when there is no private control dir (the caller then re-execs without the seamless env: a cold apply).
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

/// `write_outgoing` with the held panes' final screens: `held` names each pane kept open after its command exited, by pool id, with its visible screen, and `held_history` their scrollback's exports, each joined and named beside its screen.
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
    let mut aggregate_control = 0_u64;
    for (local_id, bytes) in controls {
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
    let mut connections_left_out = Vec::new();
    let manifest_bytes =
        |toml: &str| u64::try_from(nonce.len() + 1 + toml.len()).unwrap_or(u64::MAX);
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
    let body = format!("{nonce}\n{toml}");
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

#[cfg(unix)]
const CONNECTION_ROW_OVERHEAD: u64 = 64;

#[cfg(unix)]
fn connection_row_cost(row: &ConnectionCarry) -> u64 {
    [&row.src, &row.dst, &row.op]
        .into_iter()
        .map(|text| meta_json_manifest_cost(text.as_bytes()))
        .fold(CONNECTION_ROW_OVERHEAD, u64::saturating_add)
}

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

/// The consumed incoming handoff: the adopted live sessions plus the outgoing window's frame carry.
#[derive(Default)]
pub(crate) struct IncomingHandoff {
    pub adopted: Vec<Adopted>,
    pub window: Option<WindowCarry>,
    /// Authenticated single-use nonce retained only long enough to bind the child's eventual adoption proof to this exact parent attempt.
    pub nonce: Option<String>,
    /// Attempt-bound layout sidecar consumed from the same private nonce prefix.
    pub layout: Option<crate::restore::RestoreManifest>,
    /// [`layout_wire_digest`] over the exact sidecar bytes this process read, never a re-serialization of `layout`: the outgoing process hashed the same bytes, so this matches across a version boundary.
    pub layout_digest: Option<[u8; 32]>,
    /// [`screen_wire_digest`] over the exact carried meta/grid bytes, for the same reason.
    pub screen_digest: Option<[u8; 32]>,
    /// The manifest's tokenless connection triples, verbatim.
    pub connections: Vec<ConnectionCarry>,
    /// The unsaved Settings drafts a layout carried that this build could not place (`layout` is `None`), with how many its parse dropped.
    pub unplaced_settings_drafts: (Vec<crate::restore::SettingsDraftRestore>, usize),
    /// The final screens of the panes the outgoing process kept open after their commands exited, each read-only, matched to its layout leaf by the placeholder that stands for it (`PlaceholderLeafRestore::unhanded`).
    pub held: Vec<HeldPane>,
    /// Whose content a whole-handoff refusal was about ([`refused_whole`]); `None` for an admitted handoff and for a boot offered none.
    #[cfg_attr(not(unix), expect(dead_code, reason = "read only by the unix boot"))]
    pub refusal: Option<WholeRefusal>,
    /// The outgoing store's exit-ledger sequence ([`crate::session_store::SessionHandoff::roster_seq`]), which this process's store continues above before it registers a session.
    pub roster_seq: Option<u64>,
    /// The argv the outgoing process's bridge was attached to at runtime ([`crate::session_store::SessionHandoff::fabric_attached`]), empty when none; this process's supervisor arms it when it has no command configured.
    #[cfg_attr(not(unix), expect(dead_code, reason = "read only by the unix boot"))]
    pub fabric_attached: Vec<String>,
    /// What the outgoing status item knew of each session, by sid ([`crate::session_store::SessionRecord::status_seen`]), as the record carried it: read at each adopted session's first publication (`App::adopt_status_carry`), never a reason to refuse.
    pub status_seen: Vec<(String, String)>,
    /// Each adopted session's Autopilot end and its task's work, by sid ([`crate::session_store::SessionRecord::autopilot_end`], read by [`end_carry_of`]): the successor's `App::end_carry` as it is, spent at each adopted session's first publication (`App::adopt_end_carry`), never a reason to refuse.
    pub end_carry: std::collections::HashMap<String, crate::harness_end_carry::EndCarry>,
    /// The fleet needs the outgoing herald had told ([`crate::session_store::SessionHandoff::fleet_told`]), as the manifest carried them: read at this process's first fresh fleet read (`App::herald_fleet`), never a reason to refuse.
    pub fleet_told: Vec<String>,
    /// Where the outgoing process will name, at its Commit, how long ago a person last keyed each carried session ([`person_stamps_path`] of the manifest this process read), for the Commit to take (`App::take_carried_person_stamps`).
    #[cfg(unix)]
    pub person_stamps: Option<std::path::PathBuf>,
    /// The outgoing process's bus-mirror tombstones ([`crate::session_store::SessionHandoff::mirror_dropped`]), the rows as written; adopted into this process's store before its first session.
    pub mirror_dropped: Vec<String>,
    /// Whether the outgoing process's node mirrored ([`crate::session_store::SessionHandoff::mirror_served`]).
    pub mirror_served: bool,
}

/// Whose content a whole-handoff refusal was about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WholeRefusal {
    /// The outgoing build's artifacts: an environment that arrived in part, a descriptor list, a manifest or layout sidecar that could not be read (over the size cap: parents through v0.98.0 wrote past 4 MiB for a shell-heavy desk, which this build reads up to [`MAX_INCOMING_MANIFEST_BYTES`]), a nonce naming another attempt, a path outside the control directory.
    Producer,
    /// This build's verdict on content it did read: a manifest it cannot parse (the consumer is only ever more lenient, L3, so that is this build's to answer for), or its own adopted set disagreeing with itself.
    Own,
}

impl IncomingHandoff {
    /// How many adopted sessions arrive on a blank screen to be repainted, whichever side degraded them: the outgoing capture carried the session below its exact rungs (`ScreenCarry::repaint`), or this build refused the carried screen's content ([`admit_incoming_screen`]).
    #[must_use]
    pub(crate) fn repainted_tabs(&self) -> usize {
        self.adopted
            .iter()
            .filter(|adopted| adopted.repaint)
            .count()
    }
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

/// Overlap-handoff rollback: delete every artifact a `write_outgoing` with this `nonce` produced (all named `seamless-<pid>-<nonce>…` in our 0700 control dir, `.ctl` sidecars included).
#[cfg(any(unix, test))]
pub(crate) fn discard_outgoing(nonce: &str) {
    let Some(dir) = crate::control_auth::socket_dir() else {
        return;
    };
    discard_outgoing_in(&dir, nonce);
}

/// [`discard_outgoing`] in `dir`, the control directory it resolved.
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

/// Unlink this attempt's control-carry, history-carry and held-grid sidecars once the successor's proof has checked out.
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

/// Remove the handoff files (`seamless-<pid>-…`: manifests, layouts, grids, control sidecars) a dead process left in the control dir, and the restore machinery's hidden siblings of its layout (`.seamless-<pid>-….aterm-restore.lock` and the like, [`crate::restore`]).
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

fn handoff_leftover_pid(name: &str) -> Option<u32> {
    let name = name.strip_prefix('.').unwrap_or(name);
    let (pid, rest) = name.strip_prefix("seamless-")?.split_once('-')?;
    (!pid.is_empty() && !rest.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        .then(|| pid.parse().ok())
        .flatten()
}

/// The overlap-handoff readiness pipe's write fd, passed by the parked parent (`apply_staged_update_now`) so this process can say every carried window is painted.
pub(crate) const ENV_READY_FD: HandoffName = HandoffName::new("ATERM_HANDOFF_READY_FD");
pub(crate) const ENV_COMMIT_FD: HandoffName = HandoffName::new("ATERM_HANDOFF_COMMIT_FD");
/// Out-of-band lane: the single-use rendezvous socket the outgoing process bound before it launched this successor.
#[cfg(unix)]
pub(crate) const ENV_RENDEZVOUS: HandoffName = HandoffName::new("ATERM_HANDOFF_RENDEZVOUS");
/// The secret that admits exactly one dialer to that rendezvous.
#[cfg(unix)]
pub(crate) const ENV_CLAIM: HandoffName = HandoffName::new("ATERM_HANDOFF_CLAIM");
#[cfg(unix)]
pub(crate) const ENV_PARENT_PID: HandoffName = HandoffName::new("ATERM_HANDOFF_PARENT_PID");
/// The outgoing process's kernel birth record, published beside its pid.
#[cfg(unix)]
pub(crate) const ENV_PARENT_BIRTH: HandoffName = HandoffName::new("ATERM_HANDOFF_PARENT_BIRTH");
/// What the outgoing process can read of its successor's exit: a comma-separated capability list, published beside the parent's identity ([`outgoing_parent_env`], so both lanes carry it) and taken out of the snapshot by the successor before any shell exists ([`take_parent_reads_passing_exit_from`]).
#[cfg(unix)]
pub(crate) const ENV_EXIT_CAPS: HandoffName = HandoffName::new("ATERM_HANDOFF_EXIT_CAPS");
/// The one capability [`ENV_EXIT_CAPS`] names: this parent files a successor's [`EXIT_SUCCESSOR_PASSING`] transient and gives its counted trial launch back (`app_native::PhysicalFailureShape::of_child_death`).
#[cfg(unix)]
pub(crate) const EXIT_CAPS_PASSING: &str = "passing75";

/// The kernel's own birth record for a process: the microsecond-resolution instant `fork` created it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessBirth {
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

/// Read the kernel's record for `pid`, or `None` when there is no process we may treat as a handoff parent there.
#[cfg(target_os = "macos")]
pub(crate) fn read_process_birth(pid: libc::pid_t) -> Option<ProcessBirth> {
    let birth = aterm_uds::process::read_process_birth(pid)?;
    Some(ProcessBirth {
        seconds: birth.seconds,
        microseconds: birth.microseconds,
    })
}

/// Other unixes have no birth-record primitive wired here, so they attest only through the kernel parent link.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn read_process_birth(_pid: libc::pid_t) -> Option<ProcessBirth> {
    None
}

#[cfg(unix)]
#[must_use]
fn own_process_birth() -> Option<ProcessBirth> {
    read_process_birth(libc::pid_t::try_from(std::process::id()).ok()?)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentWitness {
    Birth(ProcessBirth),
    ForkLink,
}

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
        return None;
    }
    let live = read_process_birth(pid);
    let witness = match (published, live) {
        (Some(published), Some(live)) if published == live => ParentWitness::Birth(live),
        (Some(_), _) => return None,
        (None, Some(live)) if creator == pid => {
            if creator_after() != pid {
                return None;
            }
            ParentWitness::Birth(live)
        }
        (None, None) if creator == pid => {
            #[cfg(target_os = "macos")]
            crate::logging::stderr_line!(
                "aterm-gui: seamless handoff: no kernel birth record for parent {pid} \
                 (libproc declined it, or the parent is exiting); falling back to the \
                 process link for parent-death detection"
            );
            ParentWitness::ForkLink
        }
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

/// A handoff parent whose identity was proven at admission.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AttestedParent {
    pid: libc::pid_t,
    witness: ParentWitness,
}

#[cfg(unix)]
impl AttestedParent {
    /// The attested process's pid — a name for files it left (its crash journal, which the successor takes over at Commit), never a proof that something at that pid now is it ([`Self::still_alive`] is).
    #[must_use]
    pub(crate) fn pid(self) -> u32 {
        u32::try_from(self.pid).unwrap_or(0)
    }

    /// When the attested process started (unix nanos), from the birth record admission verified; `None` for a fork-link attestation.
    #[must_use]
    pub(crate) fn birth_unix_nanos(self) -> Option<u128> {
        match self.witness {
            ParentWitness::Birth(birth) => Some(
                u128::from(birth.seconds) * 1_000_000_000 + u128::from(birth.microseconds) * 1_000,
            ),
            ParentWitness::ForkLink => None,
        }
    }

    /// Is the attested process — that exact process, not merely something at its pid — still running?
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

    /// Wait until the attested process has exited (`true`), or until `deadline` with it still running (`false`).
    #[must_use]
    pub(crate) fn exited_by(self, deadline: std::time::Instant) -> bool {
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

    /// Is `peer`, the kernel-attested pid of a live listener this process just dialed (`LOCAL_PEERPID`, which XNU records from the listening socket at `connect(2)`), this attested process, still alive? The rendezvous's identity bind: the claim secret goes only to the parent itself, not to any same-uid process holding the published path.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub(crate) fn owns_live_listener(self, peer: u32) -> bool {
        u32::try_from(self.pid).ok() == Some(peer) && self.still_alive()
    }

    /// A lingering listener may still report its deceased creator.
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

    fn exec_env(self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        let mut pairs = vec![ENV_PARENT_PID.pair(self.pid.to_string())];
        if let ParentWitness::Birth(birth) = self.witness {
            pairs.push(ENV_PARENT_BIRTH.pair(birth.to_wire()));
        }
        pairs
    }
}

#[cfg(all(test, target_os = "macos"))]
impl AttestedParent {
    /// Attest the live process at `pid` by its current birth record, through the real admission function's no-live-creator arm — exactly how a LaunchServices successor (parent link 1) attests its outgoing process.
    pub(crate) fn attest_live_for_test(pid: libc::pid_t) -> Option<Self> {
        attest_handoff_parent_from(pid, read_process_birth(pid), 1, || 1)
    }

    /// An attestation of `pid` whose birth witness names an instant no live process there was born at: the attested parent is gone and its pid now names someone else (or no one).
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

/// The parental-authority environment an outgoing process publishes to its successor, encoded here beside the decoding in [`prearm_incoming_fds_from`] so the two cannot drift.
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

/// Whether the outgoing process declared that it reads [`EXIT_SUCCESSOR_PASSING`] as a passing failure ([`ENV_EXIT_CAPS`] names [`EXIT_CAPS_PASSING`]) — taken out of the snapshot, so no helper or shell ever sees the name.
#[cfg(unix)]
#[must_use]
pub(crate) fn take_parent_reads_passing_exit_from(env: &mut HandoffEnv) -> bool {
    env.take(ENV_EXIT_CAPS)
        .and_then(|caps| caps.into_string().ok())
        .is_some_and(|caps| caps.split(',').any(|cap| cap.trim() == EXIT_CAPS_PASSING))
}

/// The turn-id count this successor continues from, given the manifest's `next_turn_id`: past it by one more overlap reserve (`control::TURN_ID_RESERVE`).
#[must_use]
pub(crate) const fn intake_turn_floor(next_turn_id: u64) -> u64 {
    next_turn_id.saturating_add(crate::control::TURN_ID_RESERVE)
}

/// The exit status of a successor that refused the carried control-socket witness (`main_entry`'s fixed-socket check).
#[cfg(unix)]
#[must_use]
pub(crate) const fn endpoint_refusal_exit(parent_reads_passing: bool) -> i32 {
    if parent_reads_passing {
        EXIT_SUCCESSOR_PASSING
    } else {
        74
    }
}

/// Re-arm every descriptor named by an incoming handoff immediately on process entry, before the updater can spawn codesign/PlistBuddy/spctl helpers.
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

    /// Environment restored only onto the updater's final same-process exec image, never the ambient env ([`prearm_incoming_fds_from`] removed it so no codesign/PlistBuddy/spctl helper sees it).
    pub(crate) fn final_exec_env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        if !self.valid {
            return Vec::new();
        }
        self.parent
            .map(AttestedParent::exec_env)
            .unwrap_or_default()
    }

    /// Everything the boot apply's re-exec must carry onto the new image: the handoff names still in the snapshot `env`, then the attested parent's pair ([`Self::final_exec_env`]).
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

    pub(crate) const fn blocks_boot_apply(&self) -> bool {
        self.recognizable && !self.valid
    }

    /// A recognizable but malformed handoff is not an ordinary cold start.
    pub(crate) fn rejects_boot(&self) -> bool {
        self.blocks_boot_apply()
    }

    /// The attested handoff parent: named for the pid, but it carries the proof, so no later stage re-derives one from the scrubbed environment.
    pub(crate) fn parent_pid(&self) -> Option<AttestedParent> {
        self.valid.then_some(self.parent).flatten()
    }
}

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

/// [`prearm_incoming_fds_from`] over the process environment: the env-shaped entry point the handoff suites drive (`handoff_env::through_process_env`).
#[cfg(all(unix, test))]
pub(crate) fn prearm_incoming_fds() -> PrearmedIncomingFds {
    crate::handoff_env::through_process_env(PREARM_KEYS, prearm_incoming_fds_from)
}

/// The successor's entry-point posture repair, over the handoff snapshot `main_entry` captured ([`HandoffEnv::capture`]) before any helper, session, window or user code: re-arm CLOEXEC on every descriptor an incoming handoff names, attest the outgoing parent, and remove the authority names from `env`.
#[cfg(unix)]
pub(crate) fn prearm_incoming_fds_from(env: &mut HandoffEnv) -> PrearmedIncomingFds {
    let manifest_present = env.present(ENV_MANIFEST);
    let fds_present = env.present(ENV_FDS);
    let nonce_present = env.present(ENV_NONCE);
    let layout_present = env.present(ENV_LAYOUT);
    let ready_present = env.present(ENV_READY_FD);
    let commit_present = env.present(ENV_COMMIT_FD);
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

    /// The re-exec's pairs (the unix twin's doc): no handoff exists off unix, but a snapshot's stray names ride the exec as they rode `environ`.
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

/// The readiness channel owned by the incoming process.
#[cfg(unix)]
pub(crate) struct ReadySignal {
    fd: std::os::fd::OwnedFd,
    nonce: String,
    layout_digest: [u8; 32],
    screen_digest: [u8; 32],
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
    /// This process's own compiled-in identity.
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

/// Encode the parent's expectation for [`ENV_TARGET`].
#[must_use]
#[cfg(unix)]
pub(crate) fn encode_target_identity(build: u64, commit: &str) -> String {
    format!("{build} {}", commit.trim())
}

/// Read a parent's [`ENV_TARGET`] value: `"<build> <commit>"`, the commit reduced by [`normalize_commit`].
#[must_use]
pub(crate) fn parse_target_identity(raw: &str) -> Option<HandoffTarget> {
    let (build, commit) = raw.trim().split_once(' ')?;
    build
        .parse::<u64>()
        .ok()
        .zip(normalize_commit(commit))
        .map(|(build, commit)| HandoffTarget { build, commit })
}

/// Does the parent's [`ENV_TARGET`] name THIS build? A non-consuming peek for the boot-apply gate (`take_target_identity` consumes it moments later, in the same single-threaded launcher).
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

/// [`take_target_identity_from`] over the process environment: the env-shaped entry point the handoff suites drive (`handoff_env::through_process_env`).
#[cfg(all(test, unix))]
#[must_use]
pub(crate) fn take_target_identity() -> Option<HandoffTarget> {
    crate::handoff_env::through_process_env(&[ENV_TARGET], take_target_identity_from)
}

/// Consume [`ENV_TARGET`] from the handoff snapshot and prove this process is the binary the parent authorized.
#[must_use]
pub(crate) fn take_target_identity_from(env: &mut HandoffEnv) -> Option<HandoffTarget> {
    let raw = env.take(ENV_TARGET).and_then(|v| v.into_string().ok());
    let own = HandoffTarget::own();
    let Some(raw) = raw else {
        return own;
    };
    let Some(own) = own else {
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

/// The exit status a refusing successor leaves with when what stopped it was passing: `EX_TEMPFAIL` from `sysexits.h`, "temporary failure; the user is invited to retry".
#[cfg(unix)]
pub(crate) const EXIT_SUCCESSOR_PASSING: i32 = 75;

/// Whether a successor that refused its target leaves with [`EXIT_SUCCESSOR_PASSING`] instead of the ordinary refusal's clean `0`.
#[cfg(unix)]
#[must_use]
pub(crate) fn passing_refusal_exit(
    boot_apply: &aterm_update::ApplyOutcome,
    target_names_this_build: bool,
    target_admitted: bool,
    parent_reads_passing: bool,
) -> Option<i32> {
    let no_verdict_on_the_stage = match boot_apply {
        aterm_update::ApplyOutcome::NoUpdate
        | aterm_update::ApplyOutcome::NotApplicable
        | aterm_update::ApplyOutcome::LeftForSession { .. } => true,
        aterm_update::ApplyOutcome::Deferred(reason) => aterm_update::is_passing_refusal(reason),
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

/// Whether this launch was offered a handoff's sessions: the descriptor list ([`ENV_FDS`]) the fork lane's parent sets and the launched lane's granted rendezvous claim installs into the snapshot.
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

/// Whether a successor whose overlap authority is incomplete (`overlap_degraded`: a readiness or Commit channel arrived, and the pair was not admitted) must exit before any window instead of booting fresh: when it adopted sessions (they are the parked parent's too), and when it was offered some and the intake refused them all, since a cold boot would take `session.toml`, crash journals and a shell while the parent still owns every session.
#[must_use]
pub(crate) const fn overlap_intake_exits(
    overlap_degraded: bool,
    adopted: usize,
    offered: bool,
) -> bool {
    overlap_degraded && (adopted > 0 || offered)
}

/// How a successor leaves an overlap it will not take ([`overlap_intake_exits`]) once [`passing_refusal_exit`] sent no 75 for the boot swap; decided by [`refused_intake_leave`].
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusedIntakeLeave {
    /// Return from `main_entry`: the ordinary refusal's `0`, which every parent files structural, keeping the counted trial launch.
    Ordinary,
    /// Give the counted trial launch back and exit [`EXIT_SUCCESSOR_PASSING`]: the refusal was the outgoing build's content ([`WholeRefusal::Producer`]), and the parent declared that it reads 75 as a passing failure ([`ENV_EXIT_CAPS`]).
    Passing,
    /// Give the counted trial launch back and leave the ending to the parent ([`await_parents_end`]): nothing was adopted, and the parent never declared that it reads 75 (every shipped parent).
    ParentEndsIt,
}

#[cfg(unix)]
impl RefusedIntakeLeave {
    /// Whether this leave gives back the boot-trial launch this process counted ([`give_back_refused_intake_launch`]): both leaves that say the refusal is not a verdict on the bytes.
    #[must_use]
    pub(crate) const fn gives_back_trial_launch(self) -> bool {
        matches!(self, Self::Passing | Self::ParentEndsIt)
    }
}

/// How a successor whose overlap authority is incomplete leaves ([`RefusedIntakeLeave`]), from what it adopted, whose content a whole refusal was about ([`IncomingHandoff::refusal`]), and whether the parent declared that it reads [`EXIT_SUCCESSOR_PASSING`] ([`take_parent_reads_passing_exit_from`]).
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

/// Give back the boot-trial launch this process counted for `build` when `leave` says the refusal was no verdict on the bytes, by name ([`aterm_update::forgive_own_trial_launch`]), as the rendezvous and fixed-socket refusals in `main_entry` do.
#[cfg(unix)]
pub(crate) fn give_back_refused_intake_launch(leave: RefusedIntakeLeave, build: u64) -> bool {
    leave.gives_back_trial_launch() && aterm_update::forgive_own_trial_launch(build)
}

/// How long [`RefusedIntakeLeave::ParentEndsIt`] waits for the parent to end this candidate.
#[cfg(unix)]
pub(crate) const PARENTS_END_BOUND: std::time::Duration = std::time::Duration::from_secs(180);

/// How a [`await_parents_end`] wait ended, when it returned at all — the commonest end is the parent's `SIGKILL`, which returns nothing.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParentsEnd {
    /// The attested parent is gone, or none was attested: nobody is left to read this candidate's status.
    ParentGone,
    /// The bound passed with the parent still running.
    BoundPassed,
}

/// Wait, bounded by `bound`, for the attested `parent` to end this candidate ([`AttestedParent::exited_by`]).
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

    /// Compute the fixed adoption proof before either channel is touched.
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
    #[must_use]
    pub(crate) fn signal_proof(self, proof: AdoptionProof) -> bool {
        write_wire(&self.fd, &proof.to_wire())
    }
}

/// Why a successor has no Commit it can take: each way its Commit watch ([`CommitReceiver::start_watch`]), or the event loop reading the watch's answer ([`CommitReceiver::receive_commit`]), ends short of this attempt's exact Commit.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoCommit {
    /// The parent's liveness witness ([`AttestedParent::still_alive`]) no longer names it, and a last look found nothing in the pipe.
    ParentGone,
    /// `poll` answered these `revents` (`POLLHUP`, `POLLERR`, `POLLNVAL`) and nothing to read.
    PipeEnded { revents: libc::c_short },
    /// `poll` failed with this `errno`.
    PollFailed { errno: i32 },
    /// The read found end of file: every writer closed the pipe and none wrote a Commit.
    EndOfFile,
    /// The read returned `len` bytes.
    ShortCommit { len: usize },
    /// The read failed with this `errno`.
    ReadFailed { errno: i32 },
    /// A whole Commit arrived, and the event loop no longer holds the receiver it goes to.
    ReceiverGone,
    /// A whole Commit arrived that does not match this attempt's adoption proof.
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
    /// Reuse the already-attested identity when the control listener waits for this exact predecessor to exit after Commit.
    pub(crate) const fn parent(&self) -> AttestedParent {
        self.parent
    }

    const fn raw(fd: std::os::fd::OwnedFd, fail_stop: bool, parent: AttestedParent) -> Self {
        Self {
            fd: Some(fd),
            wire: None,
            fail_stop,
            parent,
        }
    }

    /// Start the parent-liveness read only after startup has completed every process-environment mutation.
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
                    // SAFETY: one initialized pollfd; the short timeout bounds parent-death detection even if another process inherited a leaked writer during Darwin's non-atomic pipe+CLOEXEC setup.
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
                    if !parent.still_alive() {
                        pollfd.revents = 0;
                        let final_poll = unsafe { libc::poll(&mut pollfd, 1, 0) };
                        if final_poll > 0 && pollfd.revents & libc::POLLIN != 0 {
                            break read_atomic_wire(&fd);
                        }
                        break Err(NoCommit::ParentGone);
                    }
                };
                drop(fd);
                if let Err(end) = received
                    && fail_stop
                {
                    fail_stop_without_commit(parent, end)
                }
                if send.send(received).is_err() && fail_stop {
                    fail_stop_without_commit(parent, NoCommit::ReceiverGone)
                }
            })
            .ok()?;
        self.wire = Some(wire);
        Some(self)
    }

    #[cfg(test)]
    pub(crate) fn for_test(fd: std::os::fd::OwnedFd) -> Self {
        // SAFETY: `getppid` is a side-effect-free libc getter.
        let creator = unsafe { libc::getppid() };
        let parent = attest_handoff_parent(creator, None)
            .expect("unit fixtures need a live parent process — run under `cargo test`");
        Self::raw(fd, false, parent)
            .start_watch()
            .expect("test commit watcher")
    }

    /// Wait for the exact attempt-bound Commit.
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

    /// A full but mismatched Commit is as terminal as EOF.
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

/// Irreversible parent Commit.
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
            crate::crash_signal::clean_exit_now(0)
        }
        if wrote < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(CommitError);
    }
}

/// Off unix the readiness signal's type is uninhabited, so `Option<ReadySignal>` is always `None` and every `Some` arm compiles away.
#[cfg(not(unix))]
pub(crate) enum ReadySignal {}
#[cfg(not(unix))]
pub(crate) enum CommitReceiver {}

#[cfg(not(unix))]
impl ReadySignal {
    /// Unreachable by construction: `match *self {}` is the compiler's own proof that no value of this type exists to have called it.
    #[must_use]
    pub(crate) fn raw_fd(&self) -> i32 {
        match *self {}
    }
}

/// [`take_ready_fd_from`] over the process environment: the env-shaped entry point the handoff suites drive (`handoff_env::through_process_env`).
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

/// Consume and validate the overlap-handoff readiness fd ([`ENV_READY_FD`]), taking it out of the handoff snapshot at once.
#[cfg(unix)]
pub(crate) fn take_ready_fd_from(
    env: &mut HandoffEnv,
    nonce: Option<String>,
    layout_digest: Option<[u8; 32]>,
    screen_digest: Option<[u8; 32]>,
    target: Option<HandoffTarget>,
    adopted_fds: &[i32],
) -> Option<ReadySignal> {
    let raw = env.take(ENV_READY_FD).and_then(|v| v.into_string().ok());
    let fd: i32 = raw?.trim().parse().ok()?;
    if fd < 3 {
        return None;
    }
    // Live-fd probe. SAFETY: `F_GETFD` on an arbitrary fd is side-effect-free.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return None;
    }
    // SAFETY: the parent created this fd solely for us; we are its sole owner now.
    let fd = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    use std::os::fd::AsRawFd as _;
    aterm_pty::set_cloexec(fd.as_raw_fd(), true).ok()?;
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

/// Consume + validate the parent-to-child Commit channel from the handoff snapshot.
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
    let raw = env.take(ENV_COMMIT_FD).and_then(|v| v.into_string().ok());
    let _nonce = nonce?;
    let parent = parent?;
    if !parent.still_alive() {
        return None;
    }
    let fd: i32 = raw?.trim().parse().ok()?;
    if fd < 3 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return None;
    }
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

/// Non-unix stub: the overlap handoff is a unix mechanism (the Windows update lane is already spawn-then-exit with no fd handoff).
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

/// The per-record hard refusals of a master this process is about to adopt, shared by both entries into the adoption landing: [`take_incoming_from`]'s handed-off masters and the keeper's recovered ones (`keeper_link::admit_recovered_master`).
pub(crate) fn admit_master_descriptor(fd: i32) -> Result<(), &'static str> {
    if !aterm_pty::fd_is_tty(fd) {
        return Err("was handed a master that is not a tty");
    }
    if aterm_pty::set_cloexec(fd, true).is_err() {
        return Err("could not re-arm CLOEXEC on its master");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PackedTabStops {
    len: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hex: Option<String>,
}

#[cfg(unix)]
const PACKED_TAB_STOPS_KEY: &str = "tab_stops_packed";

/// The stops a fresh grid `cols` wide holds, one every eighth column ([`aterm_core::grid::is_default_tab_stop`]): what a carried cursor's stops become wherever this module replaces them.
#[cfg(any(unix, test))]
pub(crate) fn default_tab_stops(cols: usize) -> Vec<bool> {
    (0..cols)
        .map(aterm_core::grid::is_default_tab_stop)
        .collect()
}

impl PackedTabStops {
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

/// [`keeper_meta_of`] of a terminal's meta carry, for tests: the pump takes the carry under the terminal's lock and builds the META after releasing it.
#[cfg(all(unix, test))]
pub(crate) fn keeper_meta(term: &aterm_core::terminal::Terminal) -> Option<Vec<u8>> {
    keeper_meta_of(term.carry_meta_abandoning_partial(), None)
}

#[cfg(unix)]
const KEEPER_FABRIC_KEY: &str = "fabric";

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

/// Build the keeper's Repaint-rung metadata JSON for a live terminal.
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

/// The keeper's Repaint rung: the checkpoint a keeper-recovered session is adopted onto, a blank canonical screen carrying the META the dead window last sent ([`keeper_meta_of`]), built by the degrade a refused handoff screen takes ([`repaint_checkpoint`]) and priced against `used_cells` across the recovery.
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

/// What a keeper-recovered session's fabric comes back as ([`recovered_fabric`]): the rows and the carry `spawn::seed_adopted_fabric` puts back, and what the META counted without naming.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct RecoveredFabric {
    pub(crate) holds: Vec<String>,
    pub(crate) topics: Vec<String>,
    pub(crate) carry: crate::fabric::FabricCarry,
    pub(crate) topics_lost: u64,
    pub(crate) queued_lost: u64,
}

/// THE FABRIC A KEEPER META CARRIED ([`crate::fabric::KeeperCarry`], under [`KEEPER_FABRIC_KEY`]), read LENIENTLY and apart from the screen, so a META whose screen does not read still gives its hold back.
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
    carry.dropped = count("dropped");
    let given_up = section
        .get("given_up")
        .and_then(|g| aterm_json::from_value::<crate::fabric::GivenUp>(g.clone()).ok());
    let given_up_log = section.get("given_up_log").and_then(Value::as_str);
    if let (Some(given_up), Some(log)) = (given_up, given_up_log) {
        carry.given_up = Some(given_up);
        carry.bus_log = Some(log.to_owned());
    }
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

/// [`take_incoming_from`] over the process environment: the env-shaped entry point the handoff suites drive (`handoff_env::through_process_env`).
#[cfg(all(test, unix))]
pub(crate) fn take_incoming() -> IncomingHandoff {
    take_incoming_as(ReceiverShape::Current)
}

/// Authenticate and consume the incoming handoff from the handoff snapshot `main_entry` captured, returning the [`Adopted`] live sessions (empty: no seamless handoff, or it failed auth; start fresh).
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

/// v0.98.0's build: the first release that reports a withheld carry per record (`SessionRecord::history_withheld`, `RELEASES.ledger`).
pub(crate) const FIRST_WITHHELD_SAYING_BUILD: u64 = 1_790_607_299;

const fn follows_policy_without_saying_so(build: u64) -> bool {
    use aterm_update_core::handoff_policy::FIRST_POLICY_FOLLOWING_BUILD;
    build >= FIRST_POLICY_FOLLOWING_BUILD && build < FIRST_WITHHELD_SAYING_BUILD
}

fn own_policy_withholds_scrollback(outgoing_build: Option<u64>) -> bool {
    outgoing_build.is_some_and(|build| {
        follows_policy_without_saying_so(build)
            && own_handoff_policy().is_some_and(|policy| {
                policy.applies_to(build) && !policy.carry_ceiling().carries_scrollback()
            })
    })
}

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
    static OWN_POLICY_FOR_TEST: std::cell::Cell<
        Option<aterm_update_core::handoff_policy::HandoffPolicy>,
    > = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn own_handoff_policy() -> Option<aterm_update_core::handoff_policy::HandoffPolicy> {
    OWN_POLICY_FOR_TEST.with(std::cell::Cell::get)
}

/// Play `policy` as this build's own sealed handoff policy on this thread, for the rest of the guard's life.
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
    if !(commit_env_present && ready_env_present && layout_path.is_some()) {
        return refused_whole(
            WholeRefusal::Producer,
            "its readiness channel, Commit channel or layout sidecar variable is missing",
        );
    }
    let Some((layout, layout_digest, unplaced_layout, unplaced_drafts)) =
        layout_path.and_then(|layout_path| {
            let layout_path = std::path::Path::new(&layout_path);
            let expected_layout = std::path::Path::new(&path).with_extension("layout.toml");
            if !layout_path.starts_with(&dir) || layout_path != expected_layout {
                return None;
            }
            let wire = take_regular_capped(layout_path, &dir, MAX_HANDOFF_LAYOUT_BYTES)
                .and_then(|bytes| String::from_utf8(bytes).ok())?;
            let digest = layout_wire_digest(&wire)?;
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
    let mut asked_for_no_scrollback: Option<bool> = manifest
        .sessions
        .iter()
        .any(|rec| rec.history.is_some())
        .then_some(false);
    let incoming = manifest
        .sessions
        .iter()
        .map(|rec| {
            let (_, fd, pid) = expected
                .iter()
                .find(|(local_id, _, _)| *local_id == rec.local_id)
                .copied()?;
            if let Err(why) = admit_master_descriptor(fd) {
                return refuse_incoming_session(rec.local_id, why);
            }
            let Some(sc) = rec.screen.as_ref() else {
                return refuse_incoming_session(rec.local_id, "carried no screen");
            };
            let meta = parse_checkpoint_meta(sc);
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
            let screen = admit_incoming_screen(meta, wire, &mut used_grid_cells);
            let repaint = sc.repaint || matches!(screen, IncomingScreen::Degraded { .. });
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
            let render_debt = rec.carried_render_debt().filter(|_| {
                !repaint
                    && checkpoint
                        .as_ref()
                        .is_some_and(|cp| cp.modes.alternate_screen)
            });
            #[cfg(unix)]
            let master = incoming_pty_guard.claim(fd);
            #[cfg(not(unix))]
            let master = crate::spawn::HandedMaster::new(fd);
            Some(IncomingSession {
                degraded,
                adopted: Adopted {
                    local_id: rec.local_id,
                    master,
                    pid,
                    sid: SessionId::new(rec.sid.clone()),
                    nonce: LaunchNonce::generate(),
                    checkpoint,
                    control,
                    repaint,
                    frozen_path: crate::session_store::predates_path_self_heal(
                        manifest.outgoing_build,
                    ) || rec.frozen_path,
                    topics: rec.topics.clone(),
                    fg_holder: rec.fg_holder.filter(|pgid| *pgid > 0).unwrap_or(0),
                    rekey: rec.rekey,
                    loader: rec.loader,
                    history,
                    holds: rec.hold_rows(),
                    title: rec.title.clone(),
                    supervisor: rec.supervisor.clone(),
                    attention_owners: rec.attention_owners.clone(),
                    claim_grace: rec.claim_grace(),
                    outgoing_build: manifest.outgoing_build,
                    keeper: None,
                    render_debt,
                    fabric: rec.fabric.clone(),
                    timeline_id: rec.timeline_id,
                    log: crate::session_log::LogAdopt {
                        ids: rec
                            .log_ids
                            .as_deref()
                            .and_then(crate::session_log::LogCarry::parse),
                        seq: rec.log_seq,
                        lines: None,
                        keeper_ep: None,
                        mirror: None,
                    },
                    journal_key: rec
                        .journal_key
                        .as_deref()
                        .and_then(crate::session_store::parse_journal_key),
                    task: rec.task.clone(),
                    autopilot_end: rec.autopilot_end.clone(),
                    wakes: rec.wakes.clone(),
                    box_receipt: rec
                        .status_seen
                        .as_deref()
                        .and_then(|row| crate::status_item::StatusCarry::parse(row).box_receipt),
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
    if let Some(why) = unplaced_layout {
        aterm_log::warn!(
            "overlap handoff: the carried layout {why}; adopting all {} session(s) as tabs of \
             the first window instead of refusing the update",
            incoming.len()
        );
    }
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
    if let Some(minted) = manifest.next_turn_id {
        let floor = intake_turn_floor(minted);
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

/// Parse each session record's carried Autopilot end and task state, keyed by `sid`.
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

/// Tier-1 conformance for `NativeUpdateSuccessorWarmBeforeClaim` (the handoff environment snapshot).
#[cfg(all(test, unix))]
#[path = "seamless_handoff_env_tests.rs"]
pub(crate) mod handoff_env_conformance;

#[cfg(all(test, unix))]
#[path = "seamless_app_capture_tests.rs"]
mod app_capture_tests;

#[cfg(test)]
#[path = "seamless_f4_adoption_proof_asymmetry_tests.rs"]
mod f4_adoption_proof_asymmetry;
