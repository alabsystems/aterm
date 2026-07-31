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
const ENV_FDS: &str = "ATERM_SEAMLESS_FDS";
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
const MAX_HANDOFF_AGGREGATE_GRID_CELLS: u64 = 128 * 1024;
const READY_WIRE_MAGIC: &[u8; 4] = b"ASR1";
const COMMIT_WIRE_MAGIC: &[u8; 4] = b"ASC1";
pub(crate) const READY_WIRE_LEN: usize = 4 + 4 + 32;

/// Whether an env set is the ONE legal handoff shape: manifest + fds + nonce +
/// layout + ready + commit, all present.
///
/// This used to answer with a three-variant `HandoffProtocolShape` whose other
/// two variants described the v0.52/v0.53 bridge. When that bridge was deleted
/// from `take_incoming`, leaving the variants here made the two functions
/// DISAGREE: a legacy-shaped env set was still judged recognizable authority
/// (so its descriptors were preserved for the exec image) and was then refused
/// on arrival. One notion of "a legal handoff", in one place, is the point.
fn handoff_is_modern_overlap(
    manifest: bool,
    fds: bool,
    nonce: bool,
    layout: bool,
    ready: bool,
    commit: bool,
) -> bool {
    manifest && fds && nonce && layout && ready && commit
}

/// One adopted session's identity in the handoff protocol: the pool-local
/// session id, the inherited PTY master fd, and the shell's pid, in that order.
/// This triple is the unit the fd channel carries, the unit
/// [`validated_identities`] proves is a bijection against the manifest, and the
/// unit [`adoption_proof`] hashes — naming it keeps those three agreeing on
/// field ORDER, which a bare tuple of three integers cannot enforce.
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

/// Canonical SHA-256 commitment to a complete adopted session set. The fd number
/// is stable across fork/exec and joins the pool id + shell pid, preventing an
/// identity-only subset from claiming a different inherited PTY.
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
const ENV_READY_FD: &str = "ATERM_HANDOFF_READY_FD";
const ENV_COMMIT_FD: &str = "ATERM_HANDOFF_COMMIT_FD";
const ENV_PARENT_PID: &str = "ATERM_HANDOFF_PARENT_PID";

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
    parent_pid: Option<libc::pid_t>,
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
        self.parent_pid
            .map(|pid| {
                vec![(
                    std::ffi::OsString::from(ENV_PARENT_PID),
                    std::ffi::OsString::from(pid.to_string()),
                )]
            })
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

    pub(crate) fn parent_pid(&self) -> Option<libc::pid_t> {
        self.valid.then_some(self.parent_pid).flatten()
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
        ENV_PARENT_PID,
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

#[cfg(unix)]
pub(crate) fn prearm_incoming_fds() -> PrearmedIncomingFds {
    let manifest_present = std::env::var_os(ENV_MANIFEST).is_some();
    let fds_present = std::env::var_os(ENV_FDS).is_some();
    let nonce_present = std::env::var_os(ENV_NONCE).is_some();
    let layout_present = std::env::var_os(ENV_LAYOUT).is_some();
    let ready_present = std::env::var_os(ENV_READY_FD).is_some();
    let commit_present = std::env::var_os(ENV_COMMIT_FD).is_some();
    let parent_present = std::env::var_os(ENV_PARENT_PID).is_some();
    let parent_pid = std::env::var(ENV_PARENT_PID)
        .ok()
        .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
        .filter(|pid| *pid > 1 && unsafe { libc::getppid() } == *pid);
    // Parent identity is now process-local typed state; never let the raw env
    // reach updater verification helpers or user shells.
    aterm_log::env::unset(ENV_PARENT_PID);
    let recognizable = manifest_present
        || fds_present
        || nonce_present
        || layout_present
        || ready_present
        || commit_present
        || parent_present;
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
    let modern = handoff_is_modern_overlap(
        manifest_present,
        fds_present,
        nonce_present,
        layout_present,
        ready_present,
        commit_present,
    );
    if recognizable && !modern {
        authority_valid = false;
    }
    if modern != parent_pid.is_some() || (parent_present && parent_pid.is_none()) {
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
        parent_pid,
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
    expected_parent_pid: libc::pid_t,
}

#[cfg(unix)]
impl CommitReceiver {
    fn raw(fd: std::os::fd::OwnedFd, fail_stop: bool, expected_parent_pid: libc::pid_t) -> Self {
        Self {
            fd: Some(fd),
            wire: None,
            fail_stop,
            expected_parent_pid,
        }
    }

    /// Start the parent-liveness read only after startup has completed every
    /// process-environment mutation. From this point, EOF at any pre-Commit cut
    /// fail-stops the readerless candidate even if the parent itself crashed.
    pub(crate) fn start_watch(mut self) -> Option<Self> {
        let fd = self.fd.take()?;
        let fail_stop = self.fail_stop;
        let expected_parent_pid = self.expected_parent_pid;
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
                    // getppid changes permanently when the direct parent exits;
                    // PID reuse cannot make this child belong to a new process.
                    if unsafe { libc::getppid() } != expected_parent_pid {
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
        Self::raw(fd, false, unsafe { libc::getppid() })
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
#[cfg(not(unix))]
pub(crate) type ReadySignal = std::convert::Infallible;
#[cfg(not(unix))]
pub(crate) type CommitReceiver = std::convert::Infallible;

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
    expected_parent_pid: Option<libc::pid_t>,
) -> Option<CommitReceiver> {
    // Read and cleared in ONE critical section (caller contract: single-threaded
    // launcher); a one-shot descriptor handoff must never be consumable twice.
    let raw = aterm_log::env::take(ENV_COMMIT_FD).and_then(|v| v.into_string().ok());
    let _nonce = nonce?;
    let expected_parent_pid = expected_parent_pid?;
    if expected_parent_pid <= 1 || unsafe { libc::getppid() } != expected_parent_pid {
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
    Some(CommitReceiver::raw(fd, true, expected_parent_pid))
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
    _expected_parent_pid: Option<i32>,
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

    #[test]
    fn protocol_shape_table_accepts_exactly_one_form() {
        for bits in 0u8..64 {
            let manifest = bits & 1 != 0;
            let fds = bits & 2 != 0;
            let nonce = bits & 4 != 0;
            let layout = bits & 8 != 0;
            let ready = bits & 16 != 0;
            let commit = bits & 32 != 0;
            let got = handoff_is_modern_overlap(manifest, fds, nonce, layout, ready, commit);
            let expected = matches!(
                (manifest, fds, nonce, layout, ready, commit),
                (true, true, true, true, true, true)
            );
            assert_eq!(got, expected, "shape bits {bits:06b}");
        }
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
        let valid = PrearmedIncomingFds {
            fds: Vec::new(),
            recognizable: true,
            valid: true,
            parent_pid: Some(4242),
        };
        assert_eq!(
            valid.final_exec_env(),
            vec![(
                std::ffi::OsString::from(ENV_PARENT_PID),
                std::ffi::OsString::from("4242"),
            )]
        );
        let invalid = PrearmedIncomingFds {
            fds: Vec::new(),
            recognizable: true,
            valid: false,
            parent_pid: Some(4242),
        };
        assert!(invalid.final_exec_env().is_empty());
        let parentless = PrearmedIncomingFds {
            fds: Vec::new(),
            recognizable: false,
            valid: true,
            parent_pid: None,
        };
        assert!(parentless.final_exec_env().is_empty());
    }

    /// THE ChildDied regression (2026-07-22): a valid OverlapModern prearm
    /// consumes `ATERM_HANDOFF_PARENT_PID` out of the ambient environment
    /// (helpers spawned afterwards must never see it) — but the boot-apply
    /// re-exec must be able to RESTORE that authority onto the successor
    /// image, or the new build's own prearm rejects the inherited handoff and
    /// the parked parent reads EOF. This proves the round trip: prearm is
    /// valid, the ambient variable is gone, and `final_exec_env` carries the
    /// exact pair the successor's prearm requires.
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

        let prearmed = prearm_incoming_fds();
        assert!(
            !prearmed.rejects_boot(),
            "the modern overlap shape is valid"
        );
        assert!(
            !prearmed.final_exec_fds().is_empty(),
            "descriptors survive for the final exec"
        );
        assert!(
            std::env::var_os(ENV_PARENT_PID).is_none(),
            "ambient parent authority is scrubbed before any helper can spawn"
        );
        assert_eq!(
            prearmed.final_exec_env(),
            vec![(
                std::ffi::OsString::from(ENV_PARENT_PID),
                std::ffi::OsString::from(parent.to_string()),
            )],
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
        let receiver = CommitReceiver::raw(read_fd, true, expected_parent)
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

    fn hex12(d: &[u8; 32]) -> String {
        d[..6].iter().map(|b| format!("{b:02x}")).collect()
    }
}
