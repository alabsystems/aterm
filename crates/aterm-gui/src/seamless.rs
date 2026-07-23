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
const MAX_HANDOFF_AGGREGATE_GRID_CELLS: u64 = 128 * 1024;
const READY_WIRE_MAGIC: &[u8; 4] = b"ASR1";
const COMMIT_WIRE_MAGIC: &[u8; 4] = b"ASC1";
pub(crate) const READY_WIRE_LEN: usize = 4 + 4 + 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffProtocolShape {
    DirectLegacy,
    OverlapLegacy,
    OverlapModern,
}

fn handoff_protocol_shape(
    manifest: bool,
    fds: bool,
    nonce: bool,
    layout: bool,
    ready: bool,
    commit: bool,
) -> Option<HandoffProtocolShape> {
    let base = manifest && fds && nonce;
    match (base, layout, ready, commit) {
        (true, false, false, false) => Some(HandoffProtocolShape::DirectLegacy),
        (true, false, true, false) => Some(HandoffProtocolShape::OverlapLegacy),
        (true, true, true, true) => Some(HandoffProtocolShape::OverlapModern),
        _ => None,
    }
}

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
    identities: &[(u64, i32, i32)],
) -> Option<AdoptionProof> {
    let count = u32::try_from(identities.len()).ok()?;
    let mut identities = identities.to_vec();
    identities.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(ADOPTION_PROOF_DOMAIN);
    hasher.update(u32::try_from(nonce.len()).ok()?.to_be_bytes());
    hasher.update(nonce.as_bytes());
    hasher.update(target_build.to_be_bytes());
    let target_commit = target_commit.trim().to_ascii_lowercase();
    let (commit_base, dirty) = target_commit
        .strip_suffix("-dirty")
        .map_or((target_commit.as_str(), false), |base| (base, true));
    let commit_prefix =
        if commit_base.as_bytes().iter().all(u8::is_ascii_hexdigit) && commit_base.len() >= 7 {
            let mut normalized = commit_base[..commit_base.len().min(12)].to_string();
            if dirty {
                normalized.push_str("-dirty");
            }
            normalized
        } else if commit_base == "unknown"
            && !dirty
            && (cfg!(debug_assertions) || std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some())
        {
            "unknown".to_string()
        } else {
            return None;
        };
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

/// Canonical, attempt-bound commitment to the complete window/tab/pane layout.
/// The same bounded serializer is validated before the parent spawns and parsed
/// by the child, so this hashes semantic handoff input rather than a path or
/// mutable global restore slot.
#[must_use]
pub(crate) fn layout_digest(layout: &crate::restore::RestoreManifest) -> Option<[u8; 32]> {
    let wire = layout.to_toml().ok()?;
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
    let mut aggregate = 0_u64;
    let mut aggregate_cells = 0_u64;
    let mut hasher = Sha256::new();
    hasher.update(SCREEN_PROOF_DOMAIN);
    hasher.update(u32::try_from(screens.len()).ok()?.to_be_bytes());
    for (local_id, checkpoint) in screens {
        let meta = CheckpointMeta::from_checkpoint(checkpoint);
        let cap = checkpoint_grid_cap(&meta)?;
        if admit_checkpoint_dimensions(
            &mut aggregate_cells,
            checkpoint.rows,
            checkpoint.cols,
            checkpoint.alt_grid.is_some(),
        )? != cap
        {
            return None;
        }
        if !checkpoint.parser_ground
            || checkpoint.alt_grid.is_some() != checkpoint.alt_cursor.is_some()
            || !checkpoint_grid_is_canonical(&checkpoint.grid, checkpoint.rows, checkpoint.cols)
        {
            return None;
        }
        let meta = serde_json::to_vec(&meta).ok()?;
        let grid_len = u64::try_from(checkpoint.grid.len()).ok()?;
        if grid_len > cap {
            return None;
        }
        aggregate = aggregate.checked_add(grid_len)?;
        hasher.update(local_id.to_be_bytes());
        hasher.update(u64::try_from(meta.len()).ok()?.to_be_bytes());
        hasher.update(&meta);
        hasher.update(grid_len.to_be_bytes());
        hasher.update(&checkpoint.grid);
        match checkpoint.alt_grid.as_ref() {
            Some(alt) => {
                let alt_len = u64::try_from(alt.len()).ok()?;
                if alt_len > cap
                    || !checkpoint_grid_is_canonical(alt, checkpoint.rows, checkpoint.cols)
                {
                    return None;
                }
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

fn take_legacy_layout_capped() -> Option<crate::restore::RestoreManifest> {
    let path = crate::restore::manifest_path()?;
    let parent = path.parent()?;
    take_regular_capped(&path, parent, MAX_HANDOFF_LAYOUT_BYTES)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|wire| crate::restore::RestoreManifest::from_toml(&wire))
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

fn parse_checkpoint_meta(carry: &ScreenCarry, legacy_bridge: bool) -> Option<CheckpointMeta> {
    if !legacy_bridge {
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
        return serde_json::from_value(value).ok();
    }
    // The one-release old-parent bridge remains tolerant of its additive v0
    // carry, but rejects unknown future schemas.
    if carry.schema > ScreenCarry::SCHEMA {
        return None;
    }
    serde_json::from_str(&carry.meta).ok()
}

fn dimension_grid_cap(rows: u16, cols: u16) -> Option<u64> {
    if rows == 0
        || cols == 0
        || rows > aterm_core::grid::MAX_GRID_ROWS
        || cols > aterm_core::grid::MAX_GRID_COLS
    {
        return None;
    }
    // Visible-only checkpoints contain exactly `rows` line-codec records. The
    // cap scales with cells plus bounded per-line framing/attribute overhead,
    // then has a protocol-wide ceiling so a maximum-dimension hostile meta can
    // never authorize a multi-gigabyte startup allocation.
    let cells = u64::from(rows).checked_mul(u64::from(cols))?;
    let cell_budget = cells.checked_mul(512)?;
    let line_budget = u64::from(rows).checked_mul(16 * 1024)?;
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
    dimension_grid_cap(meta.rows, meta.cols)
}

/// One dimension/allocation admission seam shared by UI pre-capture,
/// outgoing digest/write, and incoming decode. The aggregate changes only on
/// success, so an over-budget checkpoint cannot leave partial authority.
pub(crate) fn admit_checkpoint_dimensions(
    used_cells: &mut u64,
    rows: u16,
    cols: u16,
    has_alt: bool,
) -> Option<u64> {
    let cap = dimension_grid_cap(rows, cols)?;
    let cells = u64::from(rows).checked_mul(u64::from(cols))?;
    if cells > MAX_HANDOFF_GRID_CELLS {
        return None;
    }
    let cost = cells.checked_mul(if has_alt { 2 } else { 1 })?;
    let next = used_cells.checked_add(cost)?;
    if next > MAX_HANDOFF_AGGREGATE_GRID_CELLS {
        return None;
    }
    *used_cells = next;
    Some(cap)
}

fn checkpoint_grid_is_canonical(bytes: &[u8], rows: u16, cols: u16) -> bool {
    // A materialized grid cell holds at most one 256-byte grapheme unit. Bound
    // content and full record framing from the authenticated column count before
    // the decoder allocates any line payload or sidecars.
    let content_cap = usize::from(cols).saturating_mul(256);
    let record_cap = 16usize
        .saturating_mul(1024)
        .saturating_add(usize::from(cols).saturating_mul(512));
    let Some(lines) = aterm_core::scrollback::deserialize_lines_strict(
        bytes,
        usize::from(rows),
        usize::from(cols),
        content_cap,
        record_cap,
    ) else {
        return false;
    };
    lines.len() == usize::from(rows)
        && aterm_core::scrollback::serialize_lines(&lines).as_slice() == bytes
}

fn normalize_incoming_checkpoint_grid(
    bytes: Vec<u8>,
    rows: u16,
    cols: u16,
    legacy_v052_screen: bool,
) -> Option<Vec<u8>> {
    if legacy_v052_screen {
        // v0.52 serialized `history ++ Grid::row(0..rows)`, but Grid::row was
        // display-offset-aware and the meta omitted display_offset. With any
        // history, an unscrolled and a scrolled producer can emit an ambiguous
        // payload whose missing live-bottom rows are unrecoverable. A declared
        // line count equal to rows proves there was no history, which in turn
        // proves display_offset == 0. This is the only sound hot-bridge witness.
        let declared = bytes
            .get(..4)
            .and_then(|prefix| <[u8; 4]>::try_from(prefix).ok())
            .map(u32::from_le_bytes)
            .and_then(|count| usize::try_from(count).ok())?;
        if declared != usize::from(rows) {
            return None;
        }
    }
    checkpoint_grid_is_canonical(&bytes, rows, cols).then_some(bytes)
}

/// Validate the volatile fd channel as an exact bijection before any inherited
/// descriptor is adopted. Counts alone are insufficient: duplicate ids or fd
/// numbers can otherwise map two logical sessions onto one PTY reader.
fn validated_identities(
    manifest: &SessionHandoff,
    fds: &HandoffFds,
) -> Option<Vec<(u64, i32, i32)>> {
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

fn parse_fd_entry(entry: &str) -> Option<(u64, i32, i32)> {
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

/// Write the outgoing handoff manifest (nonce-stamped, `0600`, in the `0700` control dir)
/// and return `(manifest_path, nonce, fds_wire)` to set as env on the child `Command`.
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
) -> Option<(String, String, String)> {
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
            cp.alt_grid.is_some(),
        );
        if !cp.parser_ground
            || admitted_cap.is_none()
            || checkpoint_grid_cap(&checkpoint_meta)
                .is_none_or(|cap| u64::try_from(cp.grid.len()).map_or(true, |len| len > cap))
            || !checkpoint_grid_is_canonical(&cp.grid, cp.rows, cp.cols)
            || cp.alt_grid.is_some() != cp.alt_cursor.is_some()
            || cp.alt_grid.as_ref().is_some_and(|alt| {
                u64::try_from(alt.len()).map_or(true, |len| {
                    len > checkpoint_grid_cap(&checkpoint_meta).unwrap_or(0)
                }) || !checkpoint_grid_is_canonical(alt, cp.rows, cp.cols)
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
        rec.screen = Some(ScreenCarry {
            schema: ScreenCarry::SCHEMA,
            meta,
            grid_file: grid_file.to_string_lossy().into_owned(),
            alt_grid_file,
        });
    }
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
    Some((path.to_string_lossy().into_owned(), nonce, fds.encode()))
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
    pub expected: Option<Vec<(u64, i32, i32)>>,
    /// Attempt-bound layout sidecar consumed from the same private nonce prefix.
    pub layout: Option<crate::restore::RestoreManifest>,
    /// Canonical commitment recomputed from the exact parsed checkpoint bytes.
    pub screen_digest: Option<[u8; 32]>,
    /// True only for the one-release v0.52/v0.53 bridge: a ready channel was
    /// present, the v2 Commit and layout envs were genuinely absent, and the
    /// old global restore slot covered the exact authenticated PTY id set.
    pub legacy_layout_bridge: bool,
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
    // SAFETY: this helper is called only during the single-threaded startup seam
    // (or under the module-wide environment lock in tests), before any worker or
    // user process can concurrently observe environment mutation.
    unsafe {
        for key in [
            ENV_MANIFEST,
            ENV_FDS,
            ENV_NONCE,
            ENV_LAYOUT,
            ENV_READY_FD,
            ENV_COMMIT_FD,
            ENV_PARENT_PID,
        ] {
            std::env::remove_var(key);
        }
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
    unsafe { std::env::remove_var(ENV_PARENT_PID) };
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
    let protocol_shape = handoff_protocol_shape(
        manifest_present,
        fds_present,
        nonce_present,
        layout_present,
        ready_present,
        commit_present,
    );
    if recognizable && protocol_shape.is_none() {
        authority_valid = false;
    }
    if (protocol_shape == Some(HandoffProtocolShape::OverlapModern)) != parent_pid.is_some()
        || (parent_present && parent_pid.is_none())
    {
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
        }
    }

    /// Compute the fixed adoption proof before either channel is touched. This
    /// lets the child provision its Commit waiter before ProofReady becomes
    /// observable by the parent.
    #[must_use]
    pub(crate) fn proof(&self, adopted: &[(u64, i32, i32)]) -> Option<AdoptionProof> {
        let target_build = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
        adoption_proof(
            &self.nonce,
            target_build,
            crate::build_info::GIT_COMMIT,
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

    /// v0.52/v0.53 compatibility: their parent waits for one byte and has no
    /// Commit channel. Call only after the child independently proved its actual
    /// adopted set exactly equals the authenticated outgoing set.
    #[must_use]
    pub(crate) fn signal_legacy(self) -> bool {
        use std::os::fd::AsRawFd as _;
        let byte = [1u8];
        loop {
            // SAFETY: one-byte write from a live fixed buffer to the private pipe.
            let wrote = unsafe { libc::write(self.fd.as_raw_fd(), byte.as_ptr().cast(), 1) };
            if wrote == 1 {
                return true;
            }
            if wrote < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            return false;
        }
    }
}

/// One-release v0.52 one-channel activation bridge. The old parent has no
/// Commit channel, so a single byte is irreversible authority: emit it only
/// after exact authenticated adoption and a strictly newer child build. Reader
/// resources are already prepared behind `gate`; release is infallible and
/// happens only after the byte was written successfully.
#[cfg(unix)]
pub(crate) fn activate_legacy_bridge(
    ready: ReadySignal,
    gate: &crate::spawn::DeferredReaderGate,
    mut actual: Vec<(u64, i32, i32)>,
    mut expected: Vec<(u64, i32, i32)>,
    child_build: u64,
    parent_build: Option<u64>,
    debug_override: bool,
) -> bool {
    actual.sort_unstable();
    expected.sort_unstable();
    let activation_proved =
        debug_override || parent_build.is_some_and(|parent_build| child_build > parent_build);
    if !activation_proved || actual != expected || !ready.signal_legacy() {
        return false;
    }
    gate.release();
    true
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
    adopted_fds: &[i32],
) -> Option<ReadySignal> {
    let raw = std::env::var(ENV_READY_FD).ok();
    // SAFETY: single-threaded launcher (caller contract), so `remove_var` is sound.
    unsafe { std::env::remove_var(ENV_READY_FD) };
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
    let raw = std::env::var(ENV_COMMIT_FD).ok();
    // SAFETY: single-threaded launcher (caller contract), so `remove_var` is sound.
    unsafe { std::env::remove_var(ENV_COMMIT_FD) };
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
    // Clear immediately so the vars never reach a spawned shell, regardless of outcome.
    // SAFETY: single-threaded launcher (caller contract), so `remove_var` is sound. The
    // multithreaded test binary is the ONE exception: there every caller serializes env
    // mutation under the test module's `ENV_LOCK` instead.
    unsafe {
        std::env::remove_var(ENV_MANIFEST);
        std::env::remove_var(ENV_FDS);
        std::env::remove_var(ENV_NONCE);
        std::env::remove_var(ENV_LAYOUT);
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
    let legacy_protocol = !commit_env_present && layout_path.is_none();
    let modern_protocol = commit_env_present && ready_env_present && layout_path.is_some();
    if !legacy_protocol && !modern_protocol {
        return IncomingHandoff::default();
    }
    let layout = if legacy_protocol {
        take_legacy_layout_capped()
            .filter(|layout| legacy_layout_is_exact(Some(layout), &expected_ids))
    } else {
        layout_path.and_then(|layout_path| {
            let layout_path = std::path::Path::new(&layout_path);
            let expected_layout = std::path::Path::new(&path).with_extension("layout.toml");
            if !layout_path.starts_with(&dir) || layout_path != expected_layout {
                return None;
            }
            take_regular_capped(layout_path, &dir, MAX_HANDOFF_LAYOUT_BYTES)
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|wire| crate::restore::RestoreManifest::from_toml(&wire))
                .filter(|layout| layout.covers_exact_seamless_ids(&expected_ids))
        })
    };
    // Both direct old exec and one-channel overlap require the legacy global
    // layout to join exactly to the authenticated PTY identities. Only the
    // overlap shape emits the old one-byte ACK later.
    if layout.is_none() {
        return IncomingHandoff::default();
    }
    let legacy_layout_bridge = legacy_protocol && ready_env_present;
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
            let meta = parse_checkpoint_meta(sc, legacy_protocol)?;
            let legacy_v052_screen = legacy_protocol && sc.schema == 0;
            let grid_cap = checkpoint_grid_cap(&meta)?;
            if admit_checkpoint_dimensions(
                &mut used_grid_cells,
                meta.rows,
                meta.cols,
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
                normalize_incoming_checkpoint_grid(grid, meta.rows, meta.cols, legacy_v052_screen)?;
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
                    Some(normalize_incoming_checkpoint_grid(
                        bytes,
                        meta.rows,
                        meta.cols,
                        legacy_v052_screen,
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
    let Some(screen_refs) = adopted
        .iter()
        .map(|item| Some((item.local_id, item.checkpoint.as_ref()?)))
        .collect::<Option<Vec<_>>>()
    else {
        return IncomingHandoff::default();
    };
    let screen_digest = screen_digest_refs(screen_refs);
    let Some(screen_digest) = screen_digest else {
        return IncomingHandoff::default();
    };
    #[cfg(unix)]
    incoming_pty_guard.transfer_all();
    IncomingHandoff {
        adopted,
        window: manifest.window,
        nonce: Some(env_nonce),
        expected: Some(expected),
        layout,
        screen_digest: Some(screen_digest),
        legacy_layout_bridge,
    }
}

#[must_use]
fn legacy_layout_is_exact(
    layout: Option<&crate::restore::RestoreManifest>,
    expected_ids: &[u64],
) -> bool {
    layout.is_some_and(|layout| layout.covers_exact_seamless_ids(expected_ids))
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
            // SAFETY: mutation is serialized by ENV_LOCK, which the enclosing
            // test holds for our whole lifetime. Readers elsewhere in the
            // process may still observe the transient value — acceptable
            // because no non-seamless test depends on these vars and the prior
            // value is what we reinstate here.
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
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
    fn protocol_shape_table_accepts_only_three_exact_forms() {
        for bits in 0u8..64 {
            let manifest = bits & 1 != 0;
            let fds = bits & 2 != 0;
            let nonce = bits & 4 != 0;
            let layout = bits & 8 != 0;
            let ready = bits & 16 != 0;
            let commit = bits & 32 != 0;
            let got = handoff_protocol_shape(manifest, fds, nonce, layout, ready, commit);
            let expected = match (manifest, fds, nonce, layout, ready, commit) {
                (true, true, true, false, false, false) => Some(HandoffProtocolShape::DirectLegacy),
                (true, true, true, false, true, false) => Some(HandoffProtocolShape::OverlapLegacy),
                (true, true, true, true, true, true) => Some(HandoffProtocolShape::OverlapModern),
                _ => None,
            };
            assert_eq!(got, expected, "shape bits {bits:06b}");
        }
    }

    // Immutable bytes emitted by v0.52's `serialize_lines`. Plain lines use its
    // v3 record form: version, flags, LE content length, content, no attrs, zero
    // links. Screen metadata in that release had no `schema` field.
    const V052_NO_HISTORY_GRID_FIXTURE: &[u8] = &[
        2, 0, 0, 0, 3, 0, 9, 0, 0, 0, b'v', b'i', b's', b'i', b'b', b'l', b'e', b'-', b'0', 0, 0,
        0, 3, 0, 9, 0, 0, 0, b'v', b'i', b's', b'i', b'b', b'l', b'e', b'-', b'1', 0, 0, 0,
    ];
    const V052_SCROLLBACK_GRID_FIXTURE: &[u8] = &[
        4, 0, 0, 0, 3, 0, 9, 0, 0, 0, b'h', b'i', b's', b't', b'o', b'r', b'y', b'-', b'0', 0, 0,
        0, 3, 0, 9, 0, 0, 0, b'h', b'i', b's', b't', b'o', b'r', b'y', b'-', b'1', 0, 0, 0, 3, 0,
        9, 0, 0, 0, b'v', b'i', b's', b'i', b'b', b'l', b'e', b'-', b'0', 0, 0, 0, 3, 0, 9, 0, 0,
        0, b'v', b'i', b's', b'i', b'b', b'l', b'e', b'-', b'1', 0, 0, 0,
    ];
    // Actual v0.52 producer semantics at display_offset=1: it first serialized
    // all history, then `Grid::row(r)` appended the scrolled viewport
    // (`history-1`, `visible-0`) instead of the live (`visible-0`, `visible-1`).
    const V052_SCROLLED_PRODUCER_GRID_FIXTURE: &[u8] = &[
        4, 0, 0, 0, 3, 0, 9, 0, 0, 0, b'h', b'i', b's', b't', b'o', b'r', b'y', b'-', b'0', 0, 0,
        0, 3, 0, 9, 0, 0, 0, b'h', b'i', b's', b't', b'o', b'r', b'y', b'-', b'1', 0, 0, 0, 3, 0,
        9, 0, 0, 0, b'h', b'i', b's', b't', b'o', b'r', b'y', b'-', b'1', 0, 0, 0, 3, 0, 9, 0, 0,
        0, b'v', b'i', b's', b'i', b'b', b'l', b'e', b'-', b'0', 0, 0, 0,
    ];
    // Captured from v0.52's CheckpointMeta serializer for a terminal widened to
    // 24 columns, given a custom stop at column 22, then narrowed to 20 before
    // `visible-v052`. It has neither later saved-cursor field; the enclosing
    // ScreenCarry also had no schema key. The four retained off-width entries
    // are the real resize semantics the compatibility parser must preserve.
    const V052_CHECKPOINT_META_FIXTURE: &str = r#"{"alt_cursor":null,"charset":{"g0":"Ascii","g1":"Ascii","g1_96":null,"g2":"Ascii","g2_96":null,"g3":"Ascii","g3_96":null,"gl":"G0","gr":"G2","single_shift":"None"},"cols":20,"current_working_directory":null,"cursor":{"cursor_col":12,"cursor_row":0,"margin_left":0,"margin_right":19,"pending_wrap":false,"scroll_bottom":1,"scroll_top":0,"tab_stops":[false,false,false,false,false,false,false,false,true,false,false,false,false,false,false,false,true,false,false,false,false,false,true,false]},"kitty_keyboard":{"alt_saved_flags":null,"alt_sp":0,"alt_stack":[0,0,0,0,0,0,0,0],"flags":0,"main_saved_flags":null,"main_sp":0,"main_stack":[0,0,0,0,0,0,0,0]},"modes":{"allow_notifications":false,"allow_osc52_query":false,"allow_osc52_set":false,"allow_palette_reconfigure":false,"allow_session_memory":false,"allow_window_ops":false,"alt_send_escape":true,"alternate_screen":false,"alternate_scroll":false,"ambiguous_width_double":false,"application_cursor_keys":false,"application_keypad":false,"auto_wrap":true,"backarrow_sends_bs":false,"bidi_arrow_swap":true,"bidi_autodetection":false,"bidi_box_mirroring":false,"bidi_direction":"Auto","bidi_mode":"Implicit","bracketed_paste":false,"color_scheme":"Dark","column_mode_132":false,"cursor_blink":false,"cursor_style":"BlinkingBlock","cursor_visible":true,"deccolm_enable":false,"decncsm":false,"focus_reporting":false,"grapheme_cluster_mode":false,"in_band_size_reports":false,"insert_mode":false,"kitty_keyboard_enabled":true,"left_right_margin_mode":false,"meta_send_escape":false,"mode_1045":false,"mouse_encoding":"X10","mouse_mode":"None","new_line_mode":false,"origin_mode":false,"report_color_scheme":false,"require_shell_integration_nonce":false,"reverse_video":false,"reverse_wraparound":false,"sixel_display_mode":false,"special_modifiers":true,"stream_attribute_extent":true,"synchronized_output":false,"vt52_mode":false,"vt_level":"VT420"},"rows":2,"secure_keyboard_entry":false,"style_bg_bits":4278190080,"style_fg_bits":4294967295,"style_flag_bits":0,"style_protected":false,"taskbar_progress":null,"xterm_keyboard":{"format_other_keys":0,"modify_other_keys":0}}"#;

    fn actual_v052_manifest_fixture(grid_path: &std::path::Path) -> String {
        let grid_path = grid_path
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        format!(
            r#"schema = 1

[[sessions]]
local_id = 0
sid = "s-v052-fixture"
state = "alive"
title = "legacy shell"

[sessions.screen]
meta = '''{V052_CHECKPOINT_META_FIXTURE}'''
grid_file = "{grid_path}"

[window]
rows = 2
cols = 20
"#,
        )
    }

    #[test]
    fn actual_v052_no_history_is_the_only_sound_legacy_screen_witness() {
        let no_history =
            normalize_incoming_checkpoint_grid(V052_NO_HISTORY_GRID_FIXTURE.to_vec(), 2, 20, true)
                .expect("zero history proves v0.52 display_offset was zero");
        let lines = aterm_core::scrollback::deserialize_lines_strict(
            &no_history,
            2,
            20,
            20 * 256,
            16 * 1024 + 20 * 512,
        )
        .expect("projected modern-visible wire");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].as_str(), Some("visible-0"));
        assert_eq!(lines[1].as_str(), Some("visible-1"));

        assert!(
            normalize_incoming_checkpoint_grid(V052_SCROLLBACK_GRID_FIXTURE.to_vec(), 2, 20, true,)
                .is_none(),
            "any v0.52 history is ambiguous because display_offset was omitted"
        );
        assert!(
            normalize_incoming_checkpoint_grid(
                V052_SCROLLED_PRODUCER_GRID_FIXTURE.to_vec(),
                2,
                20,
                true,
            )
            .is_none(),
            "the genuine scrolled producer shape can never hot-adopt"
        );
        let mut trailing = V052_NO_HISTORY_GRID_FIXTURE.to_vec();
        trailing.push(0);
        assert!(
            normalize_incoming_checkpoint_grid(trailing, 2, 20, true).is_none(),
            "legacy trailing bytes are not silently ignored"
        );
        let over_count = u32::MAX.to_le_bytes().to_vec();
        assert!(
            normalize_incoming_checkpoint_grid(over_count, 2, 20, true).is_none(),
            "legacy declared line count must equal rows exactly"
        );
        let mut malformed = V052_NO_HISTORY_GRID_FIXTURE.to_vec();
        malformed[6] = u8::MAX;
        assert!(
            normalize_incoming_checkpoint_grid(malformed, 2, 20, true).is_none(),
            "malformed old records fail closed"
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
        let parsed = parse_checkpoint_meta(&carry, false).expect("modern strict meta");
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
    #[cfg(unix)]
    fn actual_v052_schema0_manifest_accepts_no_history_and_refuses_scrolled_ack() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore_xdg = RestoreVar::new("XDG_RUNTIME_DIR");
        let _restore_home = RestoreVar::new("HOME");
        let _restore_manifest = RestoreVar::new(ENV_MANIFEST);
        let _restore_fds = RestoreVar::new(ENV_FDS);
        let _restore_nonce = RestoreVar::new(ENV_NONCE);
        let _restore_layout = RestoreVar::new(ENV_LAYOUT);
        let _restore_ready = RestoreVar::new(ENV_READY_FD);
        let _restore_commit = RestoreVar::new(ENV_COMMIT_FD);
        let _restore_parent = RestoreVar::new(ENV_PARENT_PID);
        let scratch =
            std::env::temp_dir().join(format!("aterm-v052-schema0-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir(&scratch).expect("legacy scratch");
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &scratch);
            std::env::set_var("HOME", &scratch);
            std::env::remove_var(ENV_LAYOUT);
            std::env::remove_var(ENV_COMMIT_FD);
            std::env::remove_var(ENV_PARENT_PID);
        }
        let dir = crate::control_auth::socket_dir().expect("private control dir");
        std::fs::create_dir_all(&dir).expect("control dir");

        for (label, grid_wire, should_adopt) in [
            ("no-history", V052_NO_HISTORY_GRID_FIXTURE, true),
            (
                "scrolled-producer",
                V052_SCROLLED_PRODUCER_GRID_FIXTURE,
                false,
            ),
        ] {
            let (mut master, mut slave) = (0i32, 0i32);
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0,
                "legacy PTY: {label}"
            );
            crate::restore::write(&exact_legacy_layout(&[0])).expect("exact legacy layout");
            let nonce = random_nonce();
            let manifest_path = dir.join(format!("seamless-{}-{nonce}.toml", std::process::id()));
            let stem = manifest_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("manifest stem");
            let grid_path = manifest_path.with_file_name(format!("{stem}.s0.grid"));
            let toml = actual_v052_manifest_fixture(&grid_path);
            assert!(
                !toml.contains("[sessions.screen]\nschema"),
                "actual v0.52 ScreenCarry fixture has no schema field"
            );
            std::fs::write(&manifest_path, format!("{nonce}\n{toml}"))
                .expect("v0.52 manifest fixture");
            std::fs::write(&grid_path, grid_wire).expect("v0.52 grid fixture");

            let mut ready_pipe = [0i32; 2];
            assert_eq!(
                unsafe { libc::pipe(ready_pipe.as_mut_ptr()) },
                0,
                "ready pipe"
            );
            unsafe {
                std::env::set_var(ENV_MANIFEST, &manifest_path);
                std::env::set_var(ENV_NONCE, &nonce);
                std::env::set_var(ENV_FDS, format!("0={master}:4242"));
                std::env::set_var(ENV_READY_FD, ready_pipe[1].to_string());
            }
            let incoming = take_incoming();
            assert_eq!(
                incoming.adopted.len(),
                usize::from(should_adopt),
                "schema0 adoption verdict: {label}"
            );
            let adopted_fds = incoming
                .adopted
                .iter()
                .map(|adopted| adopted.master)
                .collect::<Vec<_>>();
            let ready = take_ready_fd(
                incoming.nonce.clone(),
                incoming.layout.as_ref().and_then(layout_digest),
                incoming.screen_digest,
                &adopted_fds,
            );
            assert_eq!(ready.is_some(), should_adopt, "ready proof shape: {label}");
            drop(ready);
            let mut ack = [0u8; 1];
            assert_eq!(
                unsafe { libc::read(ready_pipe[0], ack.as_mut_ptr().cast(), 1) },
                0,
                "no bridge ACK is emitted before the later exact adoption proof: {label}"
            );
            if should_adopt {
                assert!(incoming.legacy_layout_bridge);
                let checkpoint = incoming.adopted[0]
                    .checkpoint
                    .as_ref()
                    .expect("schema0 checkpoint");
                assert_eq!(checkpoint.grid.as_slice(), V052_NO_HISTORY_GRID_FIXTURE);
                assert_eq!(checkpoint.cursor.tab_stops.len(), 24);
                assert!(checkpoint.cursor.tab_stops[22]);
                let mut restored = aterm_core::terminal::Terminal::from_checkpoint(
                    checkpoint,
                    aterm_core::terminal::HostBindings::none(),
                );
                restored.resize(2, 24);
                restored.process(b"\x1b[1;18H\t");
                assert_eq!(
                    restored.cursor().col,
                    22,
                    "actual v0.52 wide->narrow carry restores its off-width custom stop"
                );
                aterm_pty::close_fd(master);
            } else {
                assert!(
                    unsafe { libc::fcntl(master, libc::F_GETFD) } < 0,
                    "rejected scrolled candidate closes its duplicate before ACK"
                );
            }
            aterm_pty::close_fd(ready_pipe[0]);
            aterm_pty::close_fd(slave);
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn decoded_cell_budget_accepts_exact_max_and_rejects_max_plus_one() {
        let mut used = 0;
        assert!(
            admit_checkpoint_dimensions(&mut used, 256, 128, false).is_some(),
            "256×128 is the exact per-grid cell ceiling"
        );
        assert_eq!(used, MAX_HANDOFF_GRID_CELLS);
        let before = used;
        assert!(
            admit_checkpoint_dimensions(&mut used, 257, 128, false).is_none(),
            "one row beyond the per-grid ceiling is rejected before capture"
        );
        assert_eq!(used, before, "failed admission is transactional");

        let mut aggregate = MAX_HANDOFF_AGGREGATE_GRID_CELLS;
        assert!(
            admit_checkpoint_dimensions(&mut aggregate, 1, 1, false).is_none(),
            "aggregate max+1 cell is rejected"
        );
        assert_eq!(aggregate, MAX_HANDOFF_AGGREGATE_GRID_CELLS);
    }

    #[test]
    fn max_admitted_visible_capture_meets_release_park_budget() {
        let (rows, cols) = (256u16, 128u16);
        let mut used = 0;
        assert!(admit_checkpoint_dimensions(&mut used, rows, cols, false).is_some());
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
        unsafe {
            std::env::remove_var(ENV_MANIFEST);
            std::env::remove_var(ENV_NONCE);
            std::env::set_var(ENV_FDS, format!("bad={named}:badpid,bad={named}:badpid"));
        }
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
        unsafe {
            std::env::set_var(ENV_MANIFEST, "/tmp/not-consumed-by-prearm");
            std::env::set_var(ENV_FDS, format!("7={aliased}:4242"));
            std::env::set_var(ENV_NONCE, "0123456789abcdef0123456789abcdef");
            std::env::remove_var(ENV_LAYOUT);
            std::env::set_var(ENV_READY_FD, aliased.to_string());
            std::env::remove_var(ENV_COMMIT_FD);
        }

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
        unsafe {
            std::env::set_var(ENV_MANIFEST, "/tmp/not-consumed-by-prearm");
            std::env::set_var(ENV_FDS, format!("7={}:4242", master_pipe[0]));
            std::env::set_var(ENV_NONCE, "0123456789abcdef0123456789abcdef");
            std::env::set_var(ENV_LAYOUT, "/tmp/not-consumed-by-prearm.layout.toml");
            std::env::set_var(ENV_READY_FD, ready_pipe[1].to_string());
            std::env::set_var(ENV_COMMIT_FD, commit_pipe[0].to_string());
            std::env::set_var(ENV_PARENT_PID, parent.to_string());
        }

        let prearmed = prearm_incoming_fds();
        assert!(!prearmed.rejects_boot(), "the modern overlap shape is valid");
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
    fn write_then_take_roundtrips_a_single_session() {
        // Serialize against the other env-touching test for the WHOLE body; a
        // poisoned lock (that test failed) still gives us the mutual exclusion
        // we need, so swallow the poison.
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        // Declared AFTER the lock guard so the restore runs BEFORE the lock is
        // released — on the happy path and on any failed assert's unwind.
        let _restore_xdg = RestoreVar::new("XDG_RUNTIME_DIR");
        let _restore_home = RestoreVar::new("HOME");
        // Isolate the control dir to a scratch 0700 so we don't touch the real one.
        let tmp = std::env::temp_dir().join(format!("aterm-seamless-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        // A tty fd to satisfy the is_tty backstop: open a real pty master.
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

        let manifest = SessionHandoff {
            schema: SessionHandoff::SCHEMA,
            window: Some(WindowCarry {
                rows: 24,
                cols: 80,
                outer_x: Some(120),
                outer_y: Some(64),
            }),
            sessions: vec![SessionRecord {
                local_id: 0,
                sid: "s-abc123".to_string(),
                parent: None,
                state: "alive".to_string(),
                title: "zsh".to_string(),
                screen: None,
                user_title: None,
                description: None,
                icon: None,
            }],
        };
        let fds = HandoffFds {
            entries: vec![(0, m, 4242)],
        };

        // SCREEN CARRY: a real engine checkpoint (styled text + scrollback +
        // cursor) must survive the write→take round-trip byte-exactly — this is
        // the "post-update window shows the exact pre-update screen" contract.
        let source_cp = {
            let mut t = aterm_core::terminal::Terminal::new(24, 80);
            t.process(b"\x1b[1;38;5;202mprompt\x1b[0m $ typed-before-update");
            for i in 0..30 {
                t.process(format!("\r\nline{i}").as_bytes());
            }
            t.checkpoint_visible()
                .expect("parser is at a command boundary")
        };
        let screens = vec![(0u64, source_cp.clone())];

        // Point socket_dir at our scratch via the documented override env.
        // SAFETY: mutation is serialized by ENV_LOCK (held for this whole test).
        // Readers elsewhere in the process may still observe the scratch value —
        // acceptable: no non-seamless test depends on ATERM_SEAMLESS_*, and
        // XDG_RUNTIME_DIR is restored by `_restore_xdg` on every exit path.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &tmp) };
        unsafe { std::env::set_var("HOME", &tmp) };
        let dir = crate::control_auth::socket_dir().expect("scratch control dir");
        std::fs::create_dir_all(&dir).ok();
        crate::restore::write(&exact_legacy_layout(&[0])).expect("legacy layout");

        let (path, nonce, wire) =
            write_outgoing(&manifest, &fds, &screens, manifest.window).expect("write handoff");
        // SAFETY: mutation serialized by ENV_LOCK (see above); `take_incoming`
        // clears these three vars again before this test releases the lock.
        unsafe {
            std::env::set_var(ENV_MANIFEST, &path);
            std::env::set_var(ENV_NONCE, &nonce);
            std::env::set_var(ENV_FDS, &wire);
        }
        let incoming = take_incoming();
        let adopted = incoming.adopted;
        assert_eq!(adopted.len(), 1, "the one session is adopted");
        assert_eq!(adopted[0].master, m, "the live master fd is handed through");
        assert_eq!(adopted[0].pid, 4242);
        assert_eq!(
            adopted[0].sid.as_str(),
            "s-abc123",
            "SID preserved across the swap"
        );
        // The screen carry round-trips byte-exactly (grid blobs via sidecar
        // files, scalars via the manifest meta) and hydrates to the same
        // checkpoint the outgoing engine captured.
        assert_eq!(
            adopted[0].checkpoint.as_ref(),
            Some(&source_cp),
            "the engine checkpoint survives the handoff byte-exactly"
        );
        assert_eq!(
            incoming.window, manifest.window,
            "the window frame carry survives"
        );
        // Every sidecar blob is consumed (single-use, like the manifest).
        let dir = crate::control_auth::socket_dir().expect("scratch control dir");
        let stray_blobs = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter(|e| e.file_name().to_string_lossy().contains(".s0."))
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(stray_blobs, 0, "screen sidecar blobs consumed on take");
        // Env is cleared + file consumed (single-use): a second take yields nothing.
        assert!(std::env::var(ENV_MANIFEST).is_err(), "env cleared");
        assert!(!std::path::Path::new(&path).exists(), "manifest consumed");
        // XDG_RUNTIME_DIR is restored (not removed) by `_restore_xdg` when this
        // scope ends — including on any failed assert above.
        for fd in [m, s] {
            unsafe { libc::close(fd) };
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// MULTI-SESSION handoff: three live shells (non-contiguous `local_id`s, out of tree
    /// order) all round-trip — each adopted with its OWN fd/pid/sid/local_id and screen
    /// carry, none lost, none cross-wired. This is the core of the multi-window seamless
    /// update: `local_id` is the bridge the incoming layout uses to place each shell back
    /// into its original pane.
    #[test]
    fn write_then_take_roundtrips_multiple_sessions() {
        let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let _restore_xdg = RestoreVar::new("XDG_RUNTIME_DIR");
        let _restore_home = RestoreVar::new("HOME");
        let tmp = std::env::temp_dir().join(format!("aterm-seamless-multi-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        // Three real pty masters (the is_tty backstop must pass for each).
        let mut masters = [0i32; 3];
        let mut slaves = [0i32; 3];
        for k in 0..3 {
            // SAFETY: valid out-params; openpty fills them on success.
            let rc = unsafe {
                libc::openpty(
                    &mut masters[k],
                    &mut slaves[k],
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(rc, 0, "openpty {k}");
        }
        // Non-contiguous ids, deliberately NOT in ascending fd order, to prove the join
        // is by `local_id` (not position): session (id=7) → master[0], (id=2) → master[1],
        // (id=9) → master[2].
        let ids = [7u64, 2, 9];
        let pids = [4242, 4243, 4244];
        let sids = ["s-seven", "s-two", "s-nine"];
        let manifest = SessionHandoff {
            schema: SessionHandoff::SCHEMA,
            window: Some(WindowCarry {
                rows: 40,
                cols: 120,
                outer_x: Some(10),
                outer_y: Some(20),
            }),
            sessions: (0..3)
                .map(|k| SessionRecord {
                    local_id: ids[k],
                    sid: sids[k].to_string(),
                    parent: None,
                    state: "alive".to_string(),
                    title: format!("shell{k}"),
                    screen: None,
                    user_title: None,
                    description: None,
                    icon: None,
                })
                .collect(),
        };
        let fds = HandoffFds {
            entries: (0..3).map(|k| (ids[k], masters[k], pids[k])).collect(),
        };
        // Each session carries a DISTINCT screen so a cross-wire would be caught.
        let screens: Vec<(u64, _)> = (0..3)
            .map(|k| {
                let mut t = aterm_core::terminal::Terminal::new(24, 80);
                t.process(format!("session {} screen \x1b[1mcontent\x1b[0m", ids[k]).as_bytes());
                (
                    ids[k],
                    t.checkpoint_visible()
                        .expect("parser is at a command boundary"),
                )
            })
            .collect();

        // SAFETY: mutation serialized by ENV_LOCK (held for the whole test).
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &tmp) };
        unsafe { std::env::set_var("HOME", &tmp) };
        let dir = crate::control_auth::socket_dir().expect("scratch control dir");
        std::fs::create_dir_all(&dir).ok();
        crate::restore::write(&exact_legacy_layout(&ids)).expect("legacy layout");

        let (path, nonce, wire) =
            write_outgoing(&manifest, &fds, &screens, manifest.window).expect("write handoff");
        // SAFETY: mutation serialized by ENV_LOCK; take_incoming clears these again.
        unsafe {
            std::env::set_var(ENV_MANIFEST, &path);
            std::env::set_var(ENV_NONCE, &nonce);
            std::env::set_var(ENV_FDS, &wire);
        }
        let incoming = take_incoming();
        let adopted = incoming.adopted;
        assert_eq!(adopted.len(), 3, "all three sessions adopt, none lost");
        // Each id maps to its OWN master/pid/sid/screen — verified by local_id lookup so
        // order can't paper over a cross-wire.
        for k in 0..3 {
            let a = adopted
                .iter()
                .find(|a| a.local_id == ids[k])
                .unwrap_or_else(|| panic!("session id {} adopted", ids[k]));
            assert_eq!(
                a.master, masters[k],
                "id {} keeps its own master fd",
                ids[k]
            );
            assert_eq!(a.pid, pids[k], "id {} keeps its own pid", ids[k]);
            assert_eq!(a.sid.as_str(), sids[k], "id {} keeps its own sid", ids[k]);
            assert_eq!(
                a.checkpoint.as_ref(),
                Some(&screens[k].1),
                "id {} keeps its own screen (no cross-wire)",
                ids[k]
            );
        }
        assert_eq!(incoming.window, manifest.window, "window frame survives");

        for k in 0..3 {
            unsafe {
                libc::close(masters[k]);
                libc::close(slaves[k]);
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
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
        unsafe {
            std::env::remove_var(ENV_MANIFEST);
            std::env::remove_var(ENV_NONCE);
            std::env::remove_var(ENV_FDS);
        }
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
        // SAFETY (all set/remove below): serialized by ENV_LOCK for the whole body.
        unsafe { std::env::remove_var("ATERM_HANDOFF_READY_FD") };
        assert!(
            take_ready_fd(Some("n".to_string()), Some([7; 32]), Some([8; 32]), &[],).is_none(),
            "absent env → None"
        );
        unsafe { std::env::set_var("ATERM_HANDOFF_READY_FD", "not-a-number") };
        assert!(
            take_ready_fd(Some("n".to_string()), Some([7; 32]), Some([8; 32]), &[],).is_none(),
            "garbage → None"
        );
        assert!(
            std::env::var_os("ATERM_HANDOFF_READY_FD").is_none(),
            "cleared on read regardless of outcome"
        );
        unsafe { std::env::set_var("ATERM_HANDOFF_READY_FD", "1") };
        assert!(
            take_ready_fd(Some("n".to_string()), Some([7; 32]), Some([8; 32]), &[],).is_none(),
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
        unsafe { std::env::set_var("ATERM_HANDOFF_READY_FD", m.to_string()) };
        assert!(
            take_ready_fd(Some("n".to_string()), Some([7; 32]), Some([8; 32]), &[],).is_none(),
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
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::set_var("ATERM_HANDOFF_READY_FD", wr.to_string()) };
        let owned = take_ready_fd(Some("n".to_string()), Some([7; 32]), Some([8; 32]), &[])
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

    #[test]
    #[cfg(unix)]
    fn v052_one_channel_bridge_requires_exact_newer_authenticated_adoption() {
        use std::os::fd::FromRawFd as _;

        let (mut master, mut slave) = (0i32, 0i32);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0,
            "authenticated PTY fixture"
        );
        let actual = vec![(7_u64, master, 4242_i32)];
        let expected = actual.clone();
        assert!(legacy_layout_is_exact(
            Some(&exact_legacy_layout(&[7])),
            &[7]
        ));
        assert!(
            !legacy_layout_is_exact(None, &[7]),
            "missing layout refused"
        );
        assert!(
            !legacy_layout_is_exact(Some(&exact_legacy_layout(&[8])), &[7]),
            "subset/mismatched legacy layout refused"
        );

        let make_ready = || {
            let mut pipe = [0i32; 2];
            assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "ready pipe");
            // SAFETY: fresh write end, solely transferred to ReadySignal.
            let write = unsafe { std::os::fd::OwnedFd::from_raw_fd(pipe[1]) };
            (
                pipe[0],
                ReadySignal::for_test(write, "legacy-v052", [3; 32], [4; 32]),
            )
        };

        let (ready_read, ready) = make_ready();
        let gate = crate::spawn::DeferredReaderGate::closed();
        assert!(activate_legacy_bridge(
            ready,
            &gate,
            actual.clone(),
            expected.clone(),
            53,
            Some(52),
            false,
        ));
        assert!(
            gate.is_released(),
            "prepared readers release after ACK only"
        );
        let mut byte = [0u8; 1];
        assert_eq!(
            unsafe { libc::read(ready_read, byte.as_mut_ptr().cast(), 1) },
            1
        );
        assert_eq!(byte[0], 1, "legacy parent receives exactly one ACK byte");
        assert_eq!(
            unsafe { libc::read(ready_read, byte.as_mut_ptr().cast(), 1) },
            0,
            "ReadySignal is consumed after its one byte"
        );
        aterm_pty::close_fd(ready_read);

        for (case, rejected_actual, rejected_expected, child, parent) in [
            ("subset", Vec::new(), expected.clone(), 53, Some(52)),
            (
                "identity mismatch",
                actual.clone(),
                vec![(7, master, 9999)],
                53,
                Some(52),
            ),
            ("not newer", actual.clone(), expected.clone(), 52, Some(52)),
        ] {
            let (ready_read, ready) = make_ready();
            let gate = crate::spawn::DeferredReaderGate::closed();
            assert!(
                !activate_legacy_bridge(
                    ready,
                    &gate,
                    rejected_actual,
                    rejected_expected,
                    child,
                    parent,
                    false,
                ),
                "{case} must refuse activation"
            );
            assert!(!gate.is_released(), "{case} keeps readers prepared/closed");
            assert_eq!(
                unsafe { libc::read(ready_read, byte.as_mut_ptr().cast(), 1) },
                0,
                "{case} emits no irreversible legacy ACK"
            );
            assert!(
                unsafe { libc::fcntl(master, libc::F_GETFD) } >= 0,
                "{case} does not close or consume the parent's PTY authority"
            );
            aterm_pty::close_fd(ready_read);
        }

        // A post-admission byte still traverses the same PTY, proving every
        // rejected bridge left kernel data ownership untouched.
        assert_eq!(unsafe { libc::write(slave, b"x".as_ptr().cast(), 1) }, 1);
        let mut pollfd = libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut pollfd, 1, 100) }, 1);
        assert_eq!(
            unsafe { libc::read(master, byte.as_mut_ptr().cast(), 1) },
            1
        );
        assert_eq!(byte[0], b'x');
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
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
        // SAFETY: serialized by ENV_LOCK; restored by `_restore_xdg`.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &tmp) };
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

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !ready.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "watcher did not arm parent-liveness detection"
                );
                std::thread::yield_now();
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
            std::thread::yield_now();
        }
        std::fs::remove_dir_all(&scratch).expect("remove parent-death scratch");
    }
}
