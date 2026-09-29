// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CROSS-VERSION GUARD (the 2026-09-22/23 update audit, plan P0-7): the
//! bytes a SHIPPED producer really wrote, adopted by THIS build's consumer.
//!
//! Every other handoff test here writes with this build and reads with this
//! build, so a consumer change that refuses what an older producer sends is
//! green in the suite and red in the field — and the field is where it costs:
//! the 0.91 → 0.92 hop is decided by 0.91's frozen producer (nothing in 0.92 can
//! change what 0.91 writes), and on 2026-09-22/23 a disagreement between one
//! build's producer and the next build's rules stranded every shell on the old
//! build for a day. So each release's producer bytes are checked in under
//! `tests/fixtures/handoff/v<release>/<desk>/` — written by that release's own
//! `write_outgoing`, in a worktree at its tag (the fixture README says how) —
//! and this guard asserts, for every release and every desk, that the current
//! consumer adopts EVERY session exactly (no repaint but the ones its producer
//! flagged, no lost control carry, each meta's state reassembled, the layout
//! placed with every Settings draft it carries) and commits to the SAME
//! digests the parent recorded, so the older parent's adoption proof matches —
//! and, for a release that carries history (v0.94.0 on), that it takes every
//! `.hist` sidecar before its proof and, after Commit, imports every line of it
//! back in front of the history the checkpoint restored. Adoption alone could
//! not see a broken import: the handoff completes, and the tab just comes back
//! with a screen and a half of its history.
//!
//! A red here is never fixed by regenerating a fixture: the bytes are what that
//! release's producer sends, forever. It is fixed by making the consumer admit
//! them again (law L3: consumer changes only ever become more lenient).

use std::sync::{Arc, Mutex, PoisonError};

use aterm_core::scrollback::Line;
use aterm_core::terminal::Terminal;

use super::tests::{ENV_LOCK, RestoreVar, child_proof_from, pipe_pair};
use super::*;

/// The fixture root: one directory per release, one per desk inside it.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/handoff");

/// Every `(release, desk)` checked in — pinned so a lost fixture directory is
/// a red test rather than a guard that silently checks less. Each release
/// appends its own rows when it adds its directory (docs/RELEASING.md).
///
/// The release cutter reads this table FROM THIS SOURCE: `ship cut` refuses
/// unless the release it succeeds has a row here
/// (`aterm-release`'s `gates::handoff_fixture_gate`). Keep it a literal table
/// of `("vX.Y.0", "<desk>")` pairs; a table the gate cannot read stops the cut.
const PINNED_DESKS: &[(&str, &str)] = &[
    ("v0.91.0", "claude-code-1049"),
    ("v0.91.0", "history"),
    ("v0.91.0", "incident-55x149"),
    ("v0.91.0", "twelve-panes"),
    ("v0.92.0", "claude-code-1049"),
    ("v0.92.0", "history"),
    ("v0.92.0", "incident-55x149"),
    ("v0.92.0", "shell-integration"),
    ("v0.92.0", "twelve-panes"),
    ("v0.93.0", "claude-code-1049"),
    ("v0.93.0", "history"),
    ("v0.93.0", "history-carry"),
    ("v0.93.0", "incident-55x149"),
    ("v0.93.0", "shell-integration"),
    ("v0.93.0", "stalled-sequence"),
    ("v0.93.0", "twelve-panes"),
    ("v0.94.0", "claude-code-1049"),
    ("v0.94.0", "history"),
    ("v0.94.0", "history-carry"),
    ("v0.94.0", "incident-55x149"),
    ("v0.94.0", "shell-integration"),
    ("v0.94.0", "twelve-panes"),
    ("v0.95.0", "claude-code-1049"),
    ("v0.95.0", "colour-and-shell"),
    ("v0.95.0", "history"),
    ("v0.95.0", "history-carry"),
    ("v0.95.0", "incident-55x149"),
    ("v0.95.0", "shell-integration"),
    ("v0.95.0", "twelve-panes"),
    ("v0.97.0", "claude-code-1049"),
    ("v0.97.0", "colour-and-shell"),
    ("v0.97.0", "history"),
    ("v0.97.0", "history-carry"),
    ("v0.97.0", "incident-55x149"),
    ("v0.97.0", "link-dense"),
    ("v0.97.0", "policy-repaint"),
    ("v0.97.0", "policy-visible"),
    ("v0.97.0", "shell-integration"),
    ("v0.97.0", "stalled-sequence"),
    ("v0.97.0", "twelve-panes"),
    ("v0.98.0", "claude-code-1049"),
    ("v0.98.0", "colour-and-shell"),
    ("v0.98.0", "history"),
    ("v0.98.0", "history-carry"),
    ("v0.98.0", "incident-55x149"),
    ("v0.98.0", "link-dense"),
    ("v0.98.0", "link-dense-history"),
    ("v0.98.0", "policy-repaint"),
    ("v0.98.0", "policy-visible"),
    ("v0.98.0", "shell-integration"),
    ("v0.98.0", "stalled-sequence"),
    ("v0.98.0", "twelve-panes"),
];

/// THE DESK SET, one `("vX.Y.0", "<desk>")` row per desk: from that release on,
/// every release must pin the desk. One desk for each producer shape a shipped
/// release has changed, so a release whose fixtures were generated in a hurry
/// cannot pass the cutter with one desk and leave the rest of its producer
/// unchecked. Before this table the cutter asked for a single pinned row,
/// which a lone `history` desk passed.
///
/// A FLOOR PER DESK, because releases are frozen. A desk added for a shape
/// release N changed is required from N on: the releases before it never
/// wrote it and never can (v0.94.0's and v0.95.0's producers never wrote
/// `stalled-sequence` — their generators asserted every parser at Ground — and
/// the policy and link-stripping desks stand for producer paths v0.97.0
/// added). One floor for the whole set, as the table first had, made adding
/// the next release's desk turn every earlier release red — the cut's guard
/// gate included — unless the floor was raised, which dropped those releases
/// from the check (the round-four review).
///
/// The release cutter reads this table FROM THIS SOURCE, like
/// [`PINNED_DESKS`] (`aterm-release`'s `gates::handoff_fixtures_of`), and so
/// does `tools/release-preflight.sh`: keep it a literal table of
/// `("vX.Y.0", "<desk>")` pairs. A desk added here is one every release from
/// its floor on must write (the fixture README says how).
const REQUIRED_DESKS: &[(&str, &str)] = &[
    ("v0.97.0", "claude-code-1049"),
    ("v0.97.0", "colour-and-shell"),
    ("v0.97.0", "history"),
    ("v0.97.0", "history-carry"),
    ("v0.97.0", "incident-55x149"),
    ("v0.97.0", "link-dense"),
    ("v0.97.0", "policy-repaint"),
    ("v0.97.0", "policy-visible"),
    ("v0.97.0", "shell-integration"),
    ("v0.97.0", "stalled-sequence"),
    ("v0.97.0", "twelve-panes"),
    ("v0.98.0", "link-dense-history"),
];

/// What one desk's producer committed to (`parent.toml`, written beside the
/// bytes by the generator in the release's worktree).
#[derive(serde::Deserialize)]
struct ParentRecord {
    producer_version: String,
    manifest: String,
    nonce: String,
    dir_placeholder: String,
    screen_digest: String,
    layout_digest: String,
    proof_target_build: u64,
    proof_target_commit: String,
    proof_identities: Vec<Vec<i64>>,
    proof_count: u32,
    proof_digest: String,
    /// The parent's `ProofReady` and `Commit` frames for that proof, as its
    /// own `to_wire`/`to_commit_wire` spelled them.
    ready_wire: String,
    commit_wire: String,
    files: Vec<String>,
    session: Vec<SessionExpectation>,
}

/// The screen the producer carried for one session.
#[derive(serde::Deserialize)]
struct SessionExpectation {
    local_id: u64,
    rows: u16,
    cols: u16,
    history_lines: u32,
    alt_grid: bool,
    control: bool,
    /// The shell-integration posture the producer carried, recorded from
    /// v0.92.0 on (the first release that carries the nonce): whether marks
    /// must be signed, and the nonce they are signed with, as 64 hex digits.
    /// Absent from older releases' `parent.toml`, which the guard then does
    /// not ask about.
    #[serde(default)]
    require_shell_integration_nonce: Option<bool>,
    #[serde(default)]
    shell_integration_nonce: Option<String>,
    /// The HISTORY CARRY the producer named, recorded from v0.94.0 on (the
    /// first release that carries it): how many lines of the history older
    /// than the checkpoint's the session's `.s<id>.hist` sidecar carries (`0`
    /// when it names none), and the session's running count of lines earlier
    /// handoffs could not carry. Absent from older releases' `parent.toml`:
    /// their producers name no sidecar and carry no count, so the guard asks
    /// for none — nothing taken, nothing imported, nothing counted.
    #[serde(default)]
    history_take: Option<u64>,
    #[serde(default)]
    history_lost: Option<u64>,
    /// The foreground holder the park stamped on the record (v0.94.0 on; `0`
    /// for none), which seeds the adopted session's reader.
    #[serde(default)]
    fg_holder: Option<i32>,
    /// THE META v0.95.0 ADDED, recorded from v0.95.0 on: the absolute row
    /// counters the successor continues (so an absolute row names the same
    /// line after the handoff), whether an application colour diff rides the
    /// meta, and the shell-integration state (the OSC 133 phase, how many
    /// command marks cross, and the completed-command count). Absent from
    /// older releases' `parent.toml`: their meta carries none of it, and the
    /// guard does not ask. [`assert_meta_state_crosses`] reads them.
    #[serde(default)]
    absolute_row_counter: Option<u64>,
    #[serde(default)]
    alt_absolute_row_counter: Option<u64>,
    #[serde(default)]
    color: Option<bool>,
    #[serde(default)]
    shell_phase: Option<u8>,
    #[serde(default)]
    shell_marks: Option<usize>,
    #[serde(default)]
    shell_completed_seq: Option<u64>,
    /// THE LADDER'S LOWER RUNGS, recorded from v0.97.0 on: whether the
    /// producer flagged the session for a repaint (its screen was carried
    /// blank, as a `carry = "repaint"` handoff policy asks), and how many
    /// lines of its history this hop left behind, counted (a policy that
    /// withholds the scrollback). Absent from older releases' `parent.toml`,
    /// whose desks repaint nothing and drop nothing. (`rung`, the ladder's own
    /// name for the carry, is recorded for review only: a consumer cannot see
    /// the rung, only these two.)
    #[serde(default)]
    repaint: Option<bool>,
    #[serde(default)]
    history_dropped: Option<u64>,
}

/// One desk directory, read.
struct Desk {
    release: String,
    name: String,
    path: std::path::PathBuf,
    parent: ParentRecord,
}

impl Desk {
    fn label(&self) -> String {
        format!("{}/{}", self.release, self.name)
    }

    fn read(&self, file: &str) -> Vec<u8> {
        std::fs::read(self.path.join(file))
            .unwrap_or_else(|error| panic!("{}: {file}: {error}", self.label()))
    }

    /// What the parent recorded for session `local_id`.
    fn expected(&self, local_id: u64) -> &SessionExpectation {
        self.parent
            .session
            .iter()
            .find(|session| session.local_id == local_id)
            .unwrap_or_else(|| panic!("{}: parent.toml has session {local_id}", self.label()))
    }

    /// The name of session `local_id`'s history sidecar beside the manifest.
    fn hist_name(&self, local_id: u64) -> String {
        format!(
            "{}.s{local_id}.hist",
            self.parent.manifest.trim_end_matches(".toml")
        )
    }

    /// The layout sidecar's name beside the manifest.
    fn layout_name(&self) -> String {
        format!(
            "{}.layout.toml",
            self.parent.manifest.trim_end_matches(".toml")
        )
    }
}

/// `N` bytes from the fixture's lowercase hex, or a panic naming the desk.
fn unhex<const N: usize>(label: &str, text: &str) -> [u8; N] {
    assert_eq!(text.len(), 2 * N, "{label}: {N} bytes of hex");
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16)
            .unwrap_or_else(|error| panic!("{label}: {error}"));
    }
    out
}

/// Every desk of every release under [`FIXTURES`], in a stable order.
fn every_desk() -> Vec<Desk> {
    let mut releases = std::fs::read_dir(FIXTURES)
        .expect("the handoff fixture root exists")
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    releases.sort_by_key(std::fs::DirEntry::file_name);
    let mut desks = Vec::new();
    for release in releases {
        let mut names = std::fs::read_dir(release.path())
            .expect("a release fixture dir")
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .collect::<Vec<_>>();
        names.sort_by_key(std::fs::DirEntry::file_name);
        for desk in names {
            let path = desk.path();
            let text = std::fs::read_to_string(path.join("parent.toml"))
                .unwrap_or_else(|error| panic!("{}: parent.toml: {error}", path.display()));
            let parent: ParentRecord = aterm_toml::from_str(&text)
                .unwrap_or_else(|error| panic!("{}: parent.toml: {error}", path.display()));
            desks.push(Desk {
                release: release.file_name().to_string_lossy().into_owned(),
                name: desk.file_name().to_string_lossy().into_owned(),
                path,
                parent,
            });
        }
    }
    desks
}

/// The producer's own commitment, recomputed from the checked-in bytes with no
/// consumer involved: the placeholder substitution touched no hashed byte (each
/// session's meta in the manifest is exactly its extracted `s<id>.meta.json`),
/// and the recorded digests are the digests of these files.
fn assert_fixture_is_self_consistent(desk: &Desk) {
    let label = desk.label();
    let body = String::from_utf8(desk.read(&desk.parent.manifest)).expect("UTF-8 manifest");
    let (file_nonce, toml) = body.split_once('\n').expect("nonce header");
    assert_eq!(
        file_nonce, desk.parent.nonce,
        "{label}: the manifest's nonce"
    );
    let manifest = SessionHandoff::from_toml(toml)
        .unwrap_or_else(|| panic!("{label}: this build parses the release's manifest"));
    let mut wire = Vec::new();
    for rec in &manifest.sessions {
        let carry = rec.screen.as_ref().expect("every session carries a screen");
        let meta = desk.read(&format!("s{}.meta.json", rec.local_id));
        assert_eq!(
            carry.meta.as_bytes(),
            meta.as_slice(),
            "{label}: session {}'s meta is the hashed bytes, untouched",
            rec.local_id
        );
        let grid_name = |path: &str| {
            path.strip_prefix(&format!("{}/", desk.parent.dir_placeholder))
                .unwrap_or_else(|| panic!("{label}: {path} names the placeholder dir"))
                .to_string()
        };
        let grid = desk.read(&grid_name(&carry.grid_file));
        let alt = carry
            .alt_grid_file
            .as_deref()
            .map(|path| desk.read(&grid_name(path)));
        wire.push((rec.local_id, meta, grid, alt));
    }
    let mut entries = wire
        .iter()
        .map(|(local_id, meta, grid, alt)| ScreenWireEntry {
            local_id: *local_id,
            meta,
            grid,
            alt_grid: alt.as_deref(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        screen_wire_digest(&mut entries),
        Some(unhex::<32>(&label, &desk.parent.screen_digest)),
        "{label}: the recorded screen digest is the digest of these bytes"
    );
    let layout = String::from_utf8(desk.read(&desk.layout_name())).expect("UTF-8 layout");
    assert_eq!(
        layout_wire_digest(&layout),
        Some(unhex::<32>(&label, &desk.parent.layout_digest)),
        "{label}: the recorded layout digest is the digest of these bytes"
    );
    // THE HISTORY CARRY the parent named (v0.94.0 on): each record's stamp,
    // `"<len> <sha256hex> <take>"`, names the depth `parent.toml` recorded,
    // and the sidecar beside it is exactly the bytes it stamps — so what the
    // guard later imports is what the parent sent. A release before the carry
    // stamps no record and ships no sidecar. Parsed here by hand, not by the
    // consumer's `parse_stamp`: this function involves no consumer.
    for rec in &manifest.sessions {
        let expected = desk.expected(rec.local_id);
        let name = desk.hist_name(rec.local_id);
        let take = expected.history_take.unwrap_or(0);
        match rec.history.as_deref() {
            Some(stamp) => {
                let fields = stamp.split(' ').collect::<Vec<_>>();
                let [len, sha, stamped_take] = fields.as_slice() else {
                    panic!("{label}: session {}'s stamp {stamp:?}", rec.local_id);
                };
                assert_eq!(
                    stamped_take.parse::<u64>().ok(),
                    Some(take).filter(|take| *take > 0),
                    "{label}: session {}'s stamp names the depth parent.toml recorded",
                    rec.local_id
                );
                let bytes = desk.read(&name);
                assert_eq!(
                    len.parse::<usize>().ok(),
                    Some(bytes.len()),
                    "{label}: session {}'s sidecar is the stamped length",
                    rec.local_id
                );
                assert_eq!(
                    unhex::<32>(&label, sha),
                    aterm_digest::Sha256::digest(&bytes),
                    "{label}: session {}'s sidecar is the stamped bytes",
                    rec.local_id
                );
                assert!(
                    desk.parent.files.contains(&name),
                    "{label}: {name} is staged with the desk"
                );
            }
            None => {
                assert_eq!(
                    take, 0,
                    "{label}: session {} names no sidecar",
                    rec.local_id
                );
                assert!(
                    !desk.parent.files.contains(&name),
                    "{label}: no sidecar {name} without a stamp"
                );
            }
        }
        assert_eq!(
            rec.history_lost,
            expected.history_lost.unwrap_or(0),
            "{label}: session {}'s running loss is the recorded one",
            rec.local_id
        );
    }
}

/// The first `take` lines of a history sidecar, read straight off the bytes
/// the parent wrote. The format is v0.94.0's (`ATHIST1`): an eight-byte
/// magic, the width the lines are wrapped at (`u16`, little endian), two zero
/// bytes, then frames of a `u32` little-endian length and a line-codec block.
/// It is decoded HERE, by the line codec alone, and not by the consumer's
/// reader: the import is what this guard checks, so it must not be what says
/// what the parent sent.
fn sidecar_history(label: &str, bytes: &[u8], take: u64) -> Vec<Line> {
    let (head, mut frames) = bytes.split_at_checked(12).expect("a sidecar header");
    assert_eq!(
        &head[..8],
        b"ATHIST1\n",
        "{label}: a v0.94.0 history sidecar"
    );
    assert_eq!(&head[10..], [0, 0], "{label}: the reserved header bytes");
    let mut lines = Vec::new();
    while let Some((size, rest)) = frames.split_first_chunk::<4>() {
        let (frame, rest) = rest
            .split_at_checked(u32::from_le_bytes(*size) as usize)
            .expect("a whole frame");
        lines.extend(aterm_core::scrollback::deserialize_lines(frame));
        frames = rest;
    }
    assert!(frames.is_empty(), "{label}: nothing after the last frame");
    let take = usize::try_from(take).expect("take");
    assert!(
        lines.len() >= take,
        "{label}: the sidecar holds the {take} line(s) it stamps"
    );
    lines.truncate(take);
    lines
}

/// Every line of `terminal`'s history-holding grid (the saved primary under
/// an alternate screen), oldest first.
fn history_lines(label: &str, terminal: &Terminal) -> Vec<Line> {
    let grid = terminal.main_grid();
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .unwrap_or_else(|| panic!("{label}: history line {i} reads back"))
                .into_owned()
        })
        .collect()
}

/// `got` is `want`, line for line: first as text (so a failure reads as the
/// lines that differ), then as each line's own line-codec encoding — every
/// cell's style, link and width — so an import that keeps the words and
/// drops the colours fails too.
fn assert_same_lines(label: &str, got: &[Line], want: &[Line], what: &str) {
    let text = |lines: &[Line]| {
        lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(text(got), text(want), "{label}: {what}");
    let wire = |lines: &[Line]| {
        lines
            .iter()
            .map(|line| aterm_core::scrollback::serialize_lines(std::slice::from_ref(line)))
            .collect::<Vec<_>>()
    };
    assert!(wire(got) == wire(want), "{label}: {what}, cell for cell");
}

/// THE PROOF FUNCTION AND ITS FRAMES ARE THE PARENT'S. The producer recorded
/// the adoption proof its own `adoption_proof` took over fixed inputs, and the
/// `ProofReady` / `Commit` frames it would put on the pipes for it; this
/// build's must be the same bytes — domain, field order, `READY_WIRE_LEN`, both
/// magics — or every parent of that release reads `AdoptionMismatch` (or never
/// sees a frame it recognizes) whatever the consumer admits.
fn assert_proof_function_is_the_parents(desk: &Desk) {
    let label = desk.label();
    let identities = desk
        .parent
        .proof_identities
        .iter()
        .map(|triple| {
            let [local_id, fd, pid] = triple.as_slice() else {
                panic!("{label}: an identity is a (local_id, fd, pid) triple");
            };
            (
                u64::try_from(*local_id).expect("local id"),
                i32::try_from(*fd).expect("fd"),
                i32::try_from(*pid).expect("pid"),
            )
        })
        .collect::<Vec<_>>();
    let proof = adoption_proof(
        &desk.parent.nonce,
        desk.parent.proof_target_build,
        &desk.parent.proof_target_commit,
        &unhex::<32>(&label, &desk.parent.layout_digest),
        &unhex::<32>(&label, &desk.parent.screen_digest),
        &identities,
    )
    .expect("a proof over the recorded inputs");
    assert_eq!(
        proof,
        AdoptionProof {
            count: desk.parent.proof_count,
            digest: unhex::<32>(&label, &desk.parent.proof_digest),
        },
        "{label}: this build's adoption proof over the parent's inputs is the parent's"
    );
    let hex = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    assert_eq!(
        hex(&proof.to_wire()),
        desk.parent.ready_wire,
        "{label}: the ProofReady frame is the one the parent reads"
    );
    assert_eq!(
        hex(&proof.to_commit_wire()),
        desk.parent.commit_wire,
        "{label}: the Commit frame is the one the parent sends"
    );
    let commit = unhex::<READY_WIRE_LEN>(&label, &desk.parent.commit_wire);
    assert!(
        proof.commit_wire_matches(&commit),
        "{label}: this build accepts the parent's Commit frame"
    );
}

/// A desk laid out for this process to adopt as a successor: the release's
/// files copied into a private control dir (never read in place — the consumer
/// deletes what it takes), fresh PTYs standing in for the parent's, and the
/// launch environment `outgoing_parent_env` and the worker would publish.
struct Staged {
    scratch: std::path::PathBuf,
    /// The private control dir the desk's files were staged into — where the
    /// consumer takes (and unlinks) each sidecar.
    dir: std::path::PathBuf,
    live: Vec<SessionIdentity>,
    slaves: Vec<i32>,
    /// The parent's ends and the Commit channel's read end: always ours.
    pipes: [i32; 3],
    /// The readiness channel's write end, until the child's `take_ready_fd`
    /// takes it (its `ReadySignal` then owns and closes it).
    ready_write: Option<i32>,
}

impl Staged {
    /// Close every descriptor still ours and remove the scratch dir. The
    /// masters are not: once the wire names them they are the successor's —
    /// the adoption closes each as it drops, and a refusal closes them at once.
    fn teardown(self) {
        for fd in self
            .slaves
            .into_iter()
            .chain(self.pipes)
            .chain(self.ready_write)
        {
            aterm_pty::close_fd(fd);
        }
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// One session's meta rewrite: the local id and what its JSON becomes.
type MetaRewrite<'a> = (u64, &'a dyn Fn(&str) -> String);

/// Stage `desk`, with `rewrite_meta` applied to the manifest's copy of the
/// named session's meta — standing in for a producer this build disagrees
/// with; the recorded parent digests then no longer describe the wire.
fn stage(desk: &Desk, rewrite_meta: Option<MetaRewrite<'_>>) -> Staged {
    let scratch = std::env::temp_dir().join(format!(
        "aterm-handoff-fixture-{}-{}-{}",
        desk.release,
        desk.name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    aterm_log::env::set("XDG_RUNTIME_DIR", &scratch);
    aterm_log::env::set("HOME", &scratch);
    let dir = crate::control_auth::socket_dir().expect("scratch control dir");
    let dir_text = dir.to_str().expect("a UTF-8 scratch dir");
    for file in &desk.parent.files {
        let bytes = desk.read(file);
        let bytes = if *file == desk.parent.manifest {
            // The manifest names each sidecar by absolute path in the parent's
            // private dir; the generator swapped that dir for a placeholder.
            // No digest covers the manifest's bytes, only the meta strings
            // inside it, which the substitution cannot reach (checked by
            // `assert_fixture_is_self_consistent`).
            let text = String::from_utf8(bytes)
                .expect("UTF-8 manifest")
                .replace(&desk.parent.dir_placeholder, dir_text);
            match rewrite_meta {
                None => text.into_bytes(),
                Some((local_id, rewrite)) => {
                    let (nonce, toml) = text.split_once('\n').expect("nonce header");
                    let mut manifest = SessionHandoff::from_toml(toml).expect("manifest");
                    let carry = manifest
                        .sessions
                        .iter_mut()
                        .find(|rec| rec.local_id == local_id)
                        .and_then(|rec| rec.screen.as_mut())
                        .expect("the rewritten session carries a screen");
                    carry.meta = rewrite(&carry.meta);
                    format!("{nonce}\n{}", manifest.to_toml().expect("reserialize")).into_bytes()
                }
            }
        } else {
            bytes
        };
        std::fs::write(dir.join(file), bytes).expect("stage a fixture file");
    }
    let mut live = Vec::new();
    let mut slaves = Vec::new();
    for (index, session) in desk.parent.session.iter().enumerate() {
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
        live.push((
            session.local_id,
            master,
            4000 + i32::try_from(index).expect("index"),
        ));
        slaves.push(slave);
    }
    let manifest_path = dir.join(&desk.parent.manifest);
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    // SAFETY: `getppid` is a side-effect-free libc getter.
    let parent_pid = unsafe { libc::getppid() };
    aterm_log::env::set(ENV_MANIFEST, &manifest_path);
    aterm_log::env::set(ENV_NONCE, &desk.parent.nonce);
    aterm_log::env::set(
        ENV_FDS,
        HandoffFds {
            entries: live.clone(),
        }
        .encode(),
    );
    aterm_log::env::set(ENV_LAYOUT, manifest_path.with_extension("layout.toml"));
    aterm_log::env::set(ENV_READY_FD, ready_write.to_string());
    aterm_log::env::set(ENV_COMMIT_FD, commit_read.to_string());
    aterm_log::env::set(ENV_PARENT_PID, parent_pid.to_string());
    match read_process_birth(parent_pid) {
        Some(birth) => aterm_log::env::set(ENV_PARENT_BIRTH, birth.to_wire()),
        None => aterm_log::env::unset(ENV_PARENT_BIRTH),
    }
    aterm_log::env::set(
        ENV_TARGET,
        encode_target_identity(
            crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
            crate::build_info::GIT_COMMIT,
        ),
    );
    Staged {
        scratch,
        dir,
        live,
        slaves,
        pipes: [ready_read, commit_read, commit_write],
        ready_write: Some(ready_write),
    }
}

/// Every variable [`stage`] and the consumer touch, restored on drop.
fn restore_env() -> [RestoreVar; 11] {
    [
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
    ]
}

/// THE HISTORY CARRY IS TAKEN, before the proof (v0.94.0 on): the sidecar
/// the parent named for `adopted` is opened whole — at the depth it stamped —
/// and unlinked, nothing is counted lost at this hop but the lines the parent
/// itself left behind and counted (`history_dropped`, v0.97.0 on: a handoff
/// policy that withholds the scrollback), and the session's running loss
/// from earlier handoffs crosses; so does the foreground holder the park
/// stamped. A release before the carry names no sidecar: nothing is taken and
/// nothing is counted.
///
/// Checked here, before the proof, and not only through the import after it:
/// the outgoing process retires every sidecar still in its dir once the proof
/// checks out, so in the field a sidecar the consumer did not open before its
/// proof is gone by the time the import runs.
fn assert_history_taken(desk: &Desk, staged: &Staged, adopted: &Adopted) {
    let label = desk.label();
    let id = adopted.local_id;
    let expected = desk.expected(id);
    let take = expected.history_take.unwrap_or(0);
    assert_eq!(
        adopted
            .history
            .carry
            .as_ref()
            .map(crate::handoff_history::HistoryCarry::take),
        Some(take).filter(|take| *take > 0),
        "{label}: session {id}'s history sidecar is taken, {take} line(s) deep"
    );
    assert_eq!(
        (adopted.history.dropped, adopted.history.lost),
        (
            expected.history_dropped.unwrap_or(0),
            expected.history_lost.unwrap_or(0)
        ),
        "{label}: session {id} loses nothing at this hop but what its parent counted, and \
         its running loss crosses"
    );
    assert!(
        !staged.dir.join(desk.hist_name(id)).exists(),
        "{label}: session {id}'s sidecar is unlinked as it is opened, before the proof"
    );
    if let Some(holder) = expected.fg_holder {
        assert_eq!(
            adopted.fg_holder, holder,
            "{label}: session {id}'s foreground holder crosses"
        );
    }
}

/// THE HISTORY LANDS, after Commit (v0.94.0 on): each adopted session is
/// hydrated as `spawn_session` hydrates it — a live engine, the checkpoint
/// restored, the import's keys reserved, the control carry installed — and
/// the import runs as the successor's worker runs it. Every line the parent
/// stamped is imported and none is lost, and the session's history is then
/// exactly the parent's sidecar lines in front of the lines the checkpoint
/// restored, oldest first, line for line and cell for cell. By the join
/// v0.94.0's producer made (its generator checked it against the parent's
/// live history) that is the whole history the parent held. A session that
/// names no sidecar (every session of a release before the carry) imports
/// nothing and keeps exactly the history its checkpoint restored.
///
/// THE META'S STATE LANDS TOO (v0.95.0 on): the engine is the one a successor
/// whose owner opted into palette changes builds (`allow_palette_reconfigure`,
/// as the `colour-and-shell` desk's parent did), and after the import every
/// colour the application set is live, the completed-command count is the
/// parent's, and every carried shell mark still names its line — a prompt
/// row that reads as a prompt (every shell on these desks prompts as
/// `dev@devhost`) and an output end that is readable — which only holds if
/// the successor continued the parent's absolute numbering through the
/// import. A repainted session is blank until its program redraws, so its
/// marks name rows nobody has drawn yet and are not read.
fn assert_history_imports(desk: &Desk, sessions: Vec<Adopted>) {
    let label = desk.label();
    let config = aterm_core::config::TerminalConfig {
        allow_palette_reconfigure: true,
        ..aterm_core::config::TerminalConfig::default()
    };
    for mut session in sessions {
        let id = session.local_id;
        let expected = desk.expected(id);
        let take = expected.history_take.unwrap_or(0);
        let dropped = expected.history_dropped.unwrap_or(0);
        let checkpoint = session
            .checkpoint
            .take()
            .unwrap_or_else(|| panic!("{label}: session {id} has its screen"));
        let mut history = std::mem::take(&mut session.history);
        let engine = Arc::new(Mutex::new(crate::spawn::new_live_terminal(
            checkpoint.rows,
            checkpoint.cols,
            Some(&config),
            aterm_types::Appearance::Dark,
            None,
        )));
        crate::spawn::hydrate_adopted_engine(
            &engine,
            Some(&checkpoint),
            session.control.take(),
            None,
            None,
            id,
            &mut history,
        );
        let restored = history_lines(
            &label,
            &engine.lock().unwrap_or_else(PoisonError::into_inner),
        );
        assert_eq!(
            restored.len(),
            checkpoint.history_lines as usize,
            "{label}: session {id} restores the history its checkpoint carries"
        );
        let report = crate::handoff_history::run_imports(vec![crate::handoff_history::ImportJob {
            session: id,
            term: Arc::clone(&engine),
            history,
        }])
        .remove(0);
        assert_eq!(
            (report.failed.as_deref(), report.cleared),
            (None, false),
            "{label}: session {id}'s import settles"
        );
        assert_eq!(
            (report.imported, report.lost()),
            (take, dropped),
            "{label}: session {id} imports every line its parent carried, and loses none but \
             what its parent counted"
        );
        let mut whole = if take > 0 {
            sidecar_history(&label, &desk.read(&desk.hist_name(id)), take)
        } else {
            Vec::new()
        };
        whole.extend(restored);
        assert_same_lines(
            &label,
            &history_lines(
                &label,
                &engine.lock().unwrap_or_else(PoisonError::into_inner),
            ),
            &whole,
            &format!(
                "session {id}'s history is its parent's sidecar in front of the lines its \
                 checkpoint restored"
            ),
        );
        assert_meta_state_lands(
            &label,
            id,
            &checkpoint,
            session.repaint,
            &engine.lock().unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// The text of absolute row `abs` of `terminal`'s history-holding grid (the
/// saved primary under an alternate screen), history or visible; `None` once
/// the row is evicted or past the screen.
fn text_at(terminal: &Terminal, abs: u64) -> Option<String> {
    let grid = terminal.main_grid();
    let oldest = grid.oldest_absolute_row();
    let top = oldest.saturating_add(u64::try_from(grid.scrollback_lines()).ok()?);
    if abs < oldest {
        return None;
    }
    if abs < top {
        return grid
            .get_history_line(usize::try_from(abs - oldest).ok()?)
            .map(|line| line.to_string().trim_end().to_string());
    }
    let row = u16::try_from(abs - top).ok()?;
    grid.row_text(row).map(|text| text.trim_end().to_string())
}

/// [`assert_history_imports`]' check of the meta's colour and shell state in
/// the hydrated, imported `engine` (see there).
fn assert_meta_state_lands(
    label: &str,
    id: u64,
    checkpoint: &aterm_core::terminal::TerminalCheckpoint,
    repaint: bool,
    engine: &Terminal,
) {
    for entry in &checkpoint.color.palette {
        assert_eq!(
            engine.palette_color(entry.index),
            entry.rgb,
            "{label}: session {id}'s palette entry {} is the application's",
            entry.index
        );
    }
    if let Some(foreground) = checkpoint.color.foreground {
        assert_eq!(
            engine.default_foreground(),
            foreground,
            "{label}: session {id}'s foreground"
        );
    }
    if let Some(background) = checkpoint.color.background {
        assert_eq!(
            engine.default_background(),
            background,
            "{label}: session {id}'s background"
        );
    }
    assert_eq!(
        engine.completed_command_seq(),
        checkpoint.shell.completed_seq,
        "{label}: session {id} goes on counting its completed commands from the parent's"
    );
    assert_eq!(
        engine.command_marks().len(),
        checkpoint.shell.command_marks.len(),
        "{label}: session {id}'s carried marks are installed"
    );
    if repaint {
        return;
    }
    for mark in engine.command_marks() {
        let prompt = text_at(engine, mark.prompt_start_row);
        assert!(
            prompt
                .as_deref()
                .is_some_and(|text| text.contains("dev@devhost")),
            "{label}: session {id}'s mark at row {} names its prompt line: {prompt:?}",
            mark.prompt_start_row
        );
        if let Some(end) = mark.output_end_row {
            assert!(
                text_at(engine, end).is_some(),
                "{label}: session {id}'s mark ends its output on a readable row {end}"
            );
        }
    }
}

/// THE META'S STATE IS REASSEMBLED (v0.95.0 on): this build reads, from the
/// older producer's meta, the absolute row counters, whether an application
/// colour diff rides it, and the shell-integration phase, marks and count
/// the parent recorded. Without this a consumer that silently dropped a meta
/// key — the numbering, the colours, a command running across the handoff —
/// adopted "exactly" by every other check here: the grids are sidecars, and
/// the digests are this build's own. A release before v0.95.0 records none
/// of these, and is asked about none.
fn assert_meta_state_crosses(
    label: &str,
    expected: &SessionExpectation,
    checkpoint: &aterm_core::terminal::TerminalCheckpoint,
) {
    let id = expected.local_id;
    if let Some(counter) = expected.absolute_row_counter {
        assert_eq!(
            checkpoint.absolute_row_counter, counter,
            "{label}: session {id}'s absolute row numbering crosses"
        );
    }
    if let Some(counter) = expected.alt_absolute_row_counter {
        assert_eq!(
            checkpoint.alt_absolute_row_counter, counter,
            "{label}: session {id}'s inactive grid's numbering crosses"
        );
    }
    if let Some(color) = expected.color {
        assert_eq!(
            !checkpoint.color.is_default(),
            color,
            "{label}: session {id}'s application colours cross"
        );
    }
    if let Some(phase) = expected.shell_phase {
        assert_eq!(
            checkpoint.shell.phase, phase,
            "{label}: session {id}'s shell-integration phase crosses"
        );
    }
    if let Some(marks) = expected.shell_marks {
        assert_eq!(
            checkpoint.shell.command_marks.len(),
            marks,
            "{label}: session {id}'s command marks cross"
        );
    }
    if let Some(seq) = expected.shell_completed_seq {
        assert_eq!(
            checkpoint.shell.completed_seq, seq,
            "{label}: session {id}'s completed-command count crosses"
        );
    }
}

/// One carried Settings draft as `(leaf, key, text)`. The leaf is its path in
/// the layout's tree, spelled `windows[w].restored_tabs[t].root`, then
/// `.first` or `.second` for each split, then `.view`: the TOML path of the
/// table that holds the draft's `settings_drafts` list.
type LeafDraft = (String, String, String);

/// Every Settings draft the parent's layout sidecar carries, with the leaf
/// that carries it — read OFF THE BYTES, never by the consumer: its lenient
/// parse, its sanitize and its per-view bound
/// (`restore::NativeLeafRestore::settings_drafts`) are what the guard checks,
/// so they must not be what says what the parent sent. Two readings that must
/// agree: the `[[….settings_drafts]]` table headers counted in the raw text
/// (how v0.98.0's producer spells them), and every `settings_drafts` list in
/// the text read as a plain TOML value tree, which gives each draft's leaf
/// and text. A draft spelled some way the count cannot see makes the two
/// disagree, so the guard stops rather than ask this build for fewer drafts
/// than the parent sent. The text comes from `aterm_toml`, the library under
/// the consumer's parse too; the count does not.
fn wire_settings_drafts(label: &str, wire: &str) -> Vec<LeafDraft> {
    fn collect(label: &str, path: &str, value: &aterm_toml::Value, out: &mut Vec<LeafDraft>) {
        match value {
            aterm_toml::Value::Table(table) => {
                for (name, value) in table {
                    if name != "settings_drafts" {
                        let path = if path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{path}.{name}")
                        };
                        collect(label, &path, value, out);
                        continue;
                    }
                    let entries = value
                        .as_array()
                        .unwrap_or_else(|| panic!("{label}: `{path}.settings_drafts` is a list"));
                    for entry in entries {
                        let field = |field: &str| {
                            entry
                                .get(field)
                                .and_then(aterm_toml::Value::as_str)
                                .unwrap_or_else(|| {
                                    panic!("{label}: a draft on {path} has a string {field}")
                                })
                                .to_string()
                        };
                        out.push((path.to_string(), field("key"), field("text")));
                    }
                }
            }
            aterm_toml::Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    collect(label, &format!("{path}[{index}]"), value, out);
                }
            }
            _ => {}
        }
    }
    let tables = wire
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("[[") && line.ends_with(".settings_drafts]]"))
        .count();
    let tree = aterm_toml::from_str::<aterm_toml::Value>(wire)
        .unwrap_or_else(|error| panic!("{label}: the layout is TOML: {error}"));
    let mut drafts = Vec::new();
    collect(label, "", &tree, &mut drafts);
    assert_eq!(
        drafts.len(),
        tables,
        "{label}: every draft in the layout's bytes is a `[[….settings_drafts]]` table the \
         guard counted"
    );
    drafts
}

/// Every Settings draft on this build's placed layout, with the leaf that
/// holds it, spelled as [`wire_settings_drafts`] spells the parent's. The
/// walk is checked against [`crate::restore::RestoreManifest::carried_settings_drafts`]
/// in [`assert_settings_drafts_placed`], so a draft this build keeps
/// somewhere the walk does not look is a red guard, not a draft it skips.
fn placed_settings_drafts(layout: &crate::restore::RestoreManifest) -> Vec<LeafDraft> {
    use crate::restore::{RestoredSplitTree, RestoredView};
    fn walk(path: String, node: &RestoredSplitTree, out: &mut Vec<LeafDraft>) {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Native(native),
            } => out.extend(native.settings_drafts.iter().map(|draft| {
                (
                    format!("{path}.view"),
                    draft.key.clone(),
                    draft.text.clone(),
                )
            })),
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                walk(format!("{path}.first"), first, out);
                walk(format!("{path}.second"), second, out);
            }
        }
    }
    let mut drafts = Vec::new();
    for (w, window) in layout.windows.iter().enumerate() {
        for (t, tab) in window.restored_tabs.iter().enumerate() {
            walk(
                format!("windows[{w}].restored_tabs[{t}].root"),
                &tab.root,
                &mut drafts,
            );
        }
    }
    drafts
}

/// THE SETTINGS DRAFTS ARE PLACED (v0.98.0 on): every unsaved Settings draft
/// the parent's layout carries ([`wire_settings_drafts`]) is on this build's
/// placed layout, on the leaf that carried it, with its key and text, and
/// none is dropped as unreadable. A consumer that tightened the drafts'
/// parse, sanitize or bound would otherwise reopen Settings without the text
/// the person typed, on every hop from that release, while the layout still
/// placed. A layout that carries no draft (every release before v0.98.0)
/// places none. Returns how many drafts the parent carried.
///
/// It checks the placed layout only: whether this build's Settings view still
/// has an input for each draft's key when the restore reopens it
/// (`App::reopen_carried_settings_drafts`) is past what a fixture can see.
fn assert_settings_drafts_placed(desk: &Desk, incoming: &IncomingHandoff) -> usize {
    let label = desk.label();
    let wire = String::from_utf8(desk.read(&desk.layout_name())).expect("UTF-8 layout");
    let mut sent = wire_settings_drafts(&label, &wire);
    let layout = incoming
        .layout
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: the layout is placed"));
    let mut placed = placed_settings_drafts(layout);
    let (carried, unreadable) = layout.carried_settings_drafts();
    assert_eq!(
        placed
            .iter()
            .map(|(_, key, text)| (key.clone(), text.clone()))
            .collect::<Vec<_>>(),
        carried
            .into_iter()
            .map(|draft| (draft.key, draft.text))
            .collect::<Vec<_>>(),
        "{label}: the guard's walk of the placed layout finds every draft this build carries"
    );
    sent.sort();
    placed.sort();
    let count = sent.len();
    assert_eq!(
        (placed, unreadable),
        (sent, 0),
        "{label}: every Settings draft the parent carried is placed on the leaf that carried \
         it, text intact, and none is dropped as unreadable"
    );
    // Not a second check while the layout places: `take_incoming` fills
    // `unplaced_settings_drafts` only when it cannot place the layout
    // (`seamless.rs`), and the layout placed (above). It keeps a consumer
    // that starts to fill it beside a placed layout red.
    assert_eq!(
        incoming.unplaced_settings_drafts,
        (Vec::new(), 0),
        "{label}: a placed layout leaves no Settings draft unplaced"
    );
    count
}

/// THE GUARD. Every desk of every shipped release adopts in THIS build
/// exactly: every session, repainting only what its producer flagged (a
/// `carry = "repaint"` policy desk, v0.97.0 on), each screen the carried
/// bytes verbatim at the carried geometry, each control carry read, each
/// meta's numbering, colour and shell state reassembled
/// ([`assert_meta_state_crosses`]), the layout placed with every Settings
/// draft it carries ([`assert_settings_drafts_placed`]) — and both digests,
/// and so the adoption proof, equal what that release's parent committed to.
/// Every history sidecar the parent named is taken before the proof
/// ([`assert_history_taken`]) and imported whole after it
/// ([`assert_history_imports`]).
#[test]
fn every_shipped_producers_desk_adopts_exactly_and_proves() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let desks = every_desk();
    let mut drafts_carried = 0usize;
    for (release, name) in PINNED_DESKS {
        assert!(
            desks
                .iter()
                .any(|desk| desk.release == *release && desk.name == *name),
            "the {release} fixture {name} is checked in"
        );
    }
    for desk in &desks {
        let label = desk.label();
        assert_eq!(
            format!("v{}", desk.parent.producer_version),
            desk.release,
            "{label}: filed under the release that wrote it"
        );
        assert_fixture_is_self_consistent(desk);
        assert_proof_function_is_the_parents(desk);

        let mut staged = stage(desk, None);
        let incoming = take_incoming_as(ReceiverShape::Current);
        assert_eq!(
            incoming.adopted.len(),
            desk.parent.session.len(),
            "{label}: every session the release handed over is adopted"
        );
        assert_eq!(
            incoming.repainted_tabs(),
            desk.parent
                .session
                .iter()
                .filter(|session| session.repaint == Some(true))
                .count(),
            "{label}: an honest desk of a shipped producer repaints exactly what its producer \
             flagged, and nothing more"
        );
        assert_eq!(
            incoming.screen_digest,
            Some(unhex::<32>(&label, &desk.parent.screen_digest)),
            "{label}: the consumer commits to the parent's screen digest"
        );
        assert_eq!(
            incoming.layout_digest,
            Some(unhex::<32>(&label, &desk.parent.layout_digest)),
            "{label}: the consumer commits to the parent's layout digest"
        );
        assert!(
            incoming.layout.is_some(),
            "{label}: the layout parses and names exactly the sessions — no orphan tabs"
        );
        drafts_carried += assert_settings_drafts_placed(desk, &incoming);
        for expected in &desk.parent.session {
            let adopted = incoming
                .adopted
                .iter()
                .find(|adopted| adopted.local_id == expected.local_id)
                .unwrap_or_else(|| panic!("{label}: session {} adopted", expected.local_id));
            let checkpoint = adopted
                .checkpoint
                .as_ref()
                .unwrap_or_else(|| panic!("{label}: session {} has its screen", adopted.local_id));
            assert_eq!(
                (
                    checkpoint.rows,
                    checkpoint.cols,
                    checkpoint.history_lines,
                    checkpoint.alt_grid.is_some(),
                ),
                (
                    expected.rows,
                    expected.cols,
                    expected.history_lines,
                    expected.alt_grid,
                ),
                "{label}: session {} keeps the carried geometry, history and inactive grid",
                adopted.local_id
            );
            assert_eq!(
                checkpoint.grid,
                desk.read(&format!(
                    "{}.s{}.grid",
                    desk.parent.manifest.trim_end_matches(".toml"),
                    adopted.local_id
                )),
                "{label}: session {}'s screen is the carried bytes",
                adopted.local_id
            );
            assert_eq!(
                adopted.control.is_some(),
                expected.control,
                "{label}: session {}'s control carry crosses",
                adopted.local_id
            );
            assert_eq!(
                adopted.repaint,
                expected.repaint.unwrap_or(false),
                "{label}: session {} is repainted exactly when its producer flagged it",
                adopted.local_id
            );
            assert_meta_state_crosses(&label, expected, checkpoint);
            // THE SHELL'S MARK AUTHORITY CROSSES: the requirement and the very
            // nonce the running shell signs with, reassembled by this build
            // from the older producer's meta — or every mark is dropped for
            // the session's life, or (worse) accepted unsigned.
            if let Some(required) = expected.require_shell_integration_nonce {
                assert_eq!(
                    checkpoint.modes.require_shell_integration_nonce, required,
                    "{label}: session {}'s nonce requirement crosses",
                    adopted.local_id
                );
                assert_eq!(
                    checkpoint
                        .shell_integration_nonce
                        .map(|nonce| nonce.to_hex()),
                    expected.shell_integration_nonce,
                    "{label}: session {}'s nonce crosses",
                    adopted.local_id
                );
            }
            assert_history_taken(desk, &staged, adopted);
        }
        let ((proof, ready, adopted), sessions) = child_proof_from(incoming)
            .unwrap_or_else(|| panic!("{label}: the child adopts and proves"));
        let expected = adoption_proof(
            &desk.parent.nonce,
            crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
            crate::build_info::GIT_COMMIT,
            &unhex::<32>(&label, &desk.parent.layout_digest),
            &unhex::<32>(&label, &desk.parent.screen_digest),
            &staged.live,
        )
        .expect("the parent's expectation");
        assert_eq!(adopted.len(), desk.parent.session.len(), "{label}");
        assert_eq!(
            proof, expected,
            "{label}: THE HANDOFF COMPLETES — the child's proof is the parent's expectation"
        );
        assert_history_imports(desk, sessions);
        drop(ready);
        staged.ready_write = None;
        staged.teardown();
    }
    assert!(
        drafts_carried > 0,
        "the guard reads at least one shipped Settings draft (v0.98.0/link-dense-history)"
    );
}

/// THE META CHECK IS NOT VACUOUS (the round-4 update audit, plan item 4), and
/// it catches what nothing else in the guard can: a CONSUMER that stops
/// reading the shell-integration state while the bytes on the wire are the
/// parent's own. The `colour-and-shell` desk of the newest release that has
/// one is staged UNMODIFIED, so both digests are the parent's and the proof
/// is its expectation; the one change is on this build's side — session 0's
/// adopted checkpoint has its shell state dropped, as a consumer that no
/// longer read the `shell` key would leave it. Every check the guard made
/// before v0.95.0's fields were read passes over it — geometry, grid bytes,
/// control carry, both digests, the proof — and [`assert_meta_state_crosses`]
/// is what fails.
///
/// Rewritten after the round-four review: the first version deleted `shell`
/// from the STAGED meta, which moves the screen digest (it hashes each meta
/// verbatim), so the guard's older digest and proof checks caught that
/// mutation on their own and the control proved nothing about the meta check.
/// Before plan item 4 the guard read none of the shell fields, so a consumer
/// dropping them, as here, passed every check it made.
#[test]
fn a_consumer_that_drops_the_shell_state_fails_only_the_meta_check() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let desk = every_desk()
        .into_iter()
        .rfind(|desk| desk.name == "colour-and-shell")
        .expect("a colour-and-shell desk");
    let label = desk.label();
    let expected = desk.expected(0);
    assert!(
        expected.shell_phase.is_some_and(|phase| phase != 0)
            && expected.shell_marks.is_some_and(|marks| marks > 0),
        "PRECONDITION: {label}'s session 0 records a shell state to lose"
    );
    let mut staged = stage(&desk, None);
    let mut incoming = take_incoming_as(ReceiverShape::Current);
    // The wire is the parent's: both digests are what it committed to.
    assert_eq!(
        incoming.screen_digest,
        Some(unhex::<32>(&label, &desk.parent.screen_digest)),
        "the bytes are the parent's own"
    );
    assert_eq!(
        incoming.layout_digest,
        Some(unhex::<32>(&label, &desk.parent.layout_digest))
    );
    let adopted = incoming
        .adopted
        .iter_mut()
        .find(|adopted| adopted.local_id == 0)
        .expect("session 0 adopts");
    let checkpoint = adopted.checkpoint.as_mut().expect("session 0's screen");
    assert_meta_state_crosses(&label, expected, checkpoint);
    // The consumer that stopped reading `shell`.
    checkpoint.shell = Default::default();
    // What the guard checked before: all of it still holds.
    assert!(!adopted.repaint, "an exact adoption");
    assert_eq!(
        (checkpoint.rows, checkpoint.cols, checkpoint.history_lines),
        (expected.rows, expected.cols, expected.history_lines),
        "the carried geometry"
    );
    assert_eq!(
        checkpoint.grid,
        desk.read(&format!(
            "{}.s0.grid",
            desk.parent.manifest.trim_end_matches(".toml")
        )),
        "the carried grid bytes"
    );
    assert_eq!(
        adopted.control.is_some(),
        expected.control,
        "the control carry"
    );
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_meta_state_crosses(&label, expected, checkpoint);
    }));
    assert!(
        caught.is_err(),
        "the guard must fail a consumer that lost the shell state its parent recorded"
    );
    // And the handoff still proves: nothing but the meta check could see it.
    let ((proof, ready, _), _sessions) =
        child_proof_from(incoming).expect("the child adopts and proves");
    let parent_expects = adoption_proof(
        &desk.parent.nonce,
        crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
        crate::build_info::GIT_COMMIT,
        &unhex::<32>(&label, &desk.parent.layout_digest),
        &unhex::<32>(&label, &desk.parent.screen_digest),
        &staged.live,
    )
    .expect("the parent's expectation");
    assert_eq!(proof, parent_expects, "{label}: the proof is the parent's");
    drop(ready);
    staged.ready_write = None;
    staged.teardown();
}

/// The first Settings leaf of `layout` that holds a carried draft.
fn settings_leaf_with_drafts(
    layout: &mut crate::restore::RestoreManifest,
) -> &mut crate::restore::NativeLeafRestore {
    use crate::restore::{NativeLeafRestore, RestoredSplitTree, RestoredView};
    fn find(node: &mut RestoredSplitTree) -> Option<&mut NativeLeafRestore> {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Native(native),
            } if !native.settings_drafts.is_empty() => Some(native),
            RestoredSplitTree::Leaf { .. } => None,
            RestoredSplitTree::Split { first, second, .. } => find(first).or_else(|| find(second)),
        }
    }
    layout
        .windows
        .iter_mut()
        .flat_map(|window| window.restored_tabs.iter_mut())
        .find_map(|tab| find(&mut tab.root))
        .expect("a Settings leaf that holds a carried draft")
}

/// THE DRAFTS CHECK IS NOT VACUOUS: [`assert_settings_drafts_placed`] fails a
/// consumer that loses a carried Settings draft, or places it on another
/// leaf, while the bytes on the wire are the parent's own. The newest
/// `link-dense-history` desk (a Settings tab with one unsaved draft, v0.98.0
/// on) is staged UNMODIFIED, so both digests are the parent's, the layout
/// places and the proof is its expectation; each change is to this build's
/// placed layout, and each one is put back before the next:
///
/// - the draft dropped without a count, as a sanitize that cleared the list
///   would leave it;
/// - the draft dropped and counted unreadable, as a tightened
///   `settings_draft_is_carriable` or bound would leave it;
/// - the Settings tab placed in the other tab's position. Every draft's key
///   and text are still on the layout, so a check of the layout's drafts as
///   one list passes it; the leaf is what differs.
#[test]
fn a_consumer_that_loses_or_moves_a_settings_draft_fails_the_drafts_check() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let desk = every_desk()
        .into_iter()
        .rfind(|desk| desk.name == "link-dense-history")
        .expect("a link-dense-history desk");
    let label = desk.label();
    let wire = String::from_utf8(desk.read(&desk.layout_name())).expect("UTF-8 layout");
    assert!(
        !wire_settings_drafts(&label, &wire).is_empty(),
        "PRECONDITION: {label}'s layout carries a Settings draft to lose"
    );
    let mut staged = stage(&desk, None);
    let mut incoming = take_incoming_as(ReceiverShape::Current);
    assert_eq!(
        incoming.layout_digest,
        Some(unhex::<32>(&label, &desk.parent.layout_digest)),
        "the layout bytes are the parent's own"
    );
    assert!(
        assert_settings_drafts_placed(&desk, &incoming) > 0,
        "the unmodified desk passes"
    );
    let original = incoming.layout.clone().expect("the layout places");
    let fails = |incoming: &IncomingHandoff| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_settings_drafts_placed(&desk, incoming);
        }))
        .is_err()
    };

    // Dropped, uncounted.
    let layout = incoming.layout.as_mut().expect("the layout places");
    settings_leaf_with_drafts(layout).settings_drafts.clear();
    assert!(
        fails(&incoming),
        "the guard must fail a consumer that dropped a carried draft uncounted"
    );

    // Dropped, counted unreadable.
    incoming.layout = Some(original.clone());
    let layout = incoming.layout.as_mut().expect("the layout places");
    let leaf = settings_leaf_with_drafts(layout);
    let dropped = std::mem::take(&mut leaf.settings_drafts).len();
    leaf.settings_drafts_unreadable = dropped;
    assert_eq!(layout.carried_settings_drafts(), (Vec::new(), dropped));
    assert!(
        fails(&incoming),
        "the guard must fail a consumer that could not read a carried draft"
    );

    // Placed on another leaf.
    incoming.layout = Some(original.clone());
    let layout = incoming.layout.as_mut().expect("the layout places");
    assert_eq!(
        layout
            .windows
            .first()
            .map(|window| window.restored_tabs.len()),
        Some(2),
        "PRECONDITION: {label}'s window holds a terminal tab and a Settings tab"
    );
    layout.windows[0].restored_tabs.swap(0, 1);
    assert_eq!(
        layout.carried_settings_drafts(),
        original.carried_settings_drafts(),
        "every draft's key and text are still on the layout"
    );
    assert_ne!(
        placed_settings_drafts(layout),
        placed_settings_drafts(&original),
        "the draft is on another leaf"
    );
    assert!(
        fails(&incoming),
        "the guard must fail a consumer that placed a carried draft on another leaf"
    );

    // Put back, the desk passes and the handoff proves.
    incoming.layout = Some(original);
    assert!(
        assert_settings_drafts_placed(&desk, &incoming) > 0,
        "{label}"
    );
    let ((proof, ready, _), _sessions) =
        child_proof_from(incoming).expect("the child adopts and proves");
    let parent_expects = adoption_proof(
        &desk.parent.nonce,
        crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
        crate::build_info::GIT_COMMIT,
        &unhex::<32>(&label, &desk.parent.layout_digest),
        &unhex::<32>(&label, &desk.parent.screen_digest),
        &staged.live,
    )
    .expect("the parent's expectation");
    assert_eq!(proof, parent_expects, "{label}: the proof is the parent's");
    drop(ready);
    staged.ready_write = None;
    staged.teardown();
}

/// A `vX.Y.0` fixture label as `(X, Y, 0)`, for ordering releases.
fn release_triple(label: &str) -> (u64, u64, u64) {
    let parts = label
        .strip_prefix('v')
        .unwrap_or_else(|| panic!("{label}: a vX.Y.Z release"))
        .split('.')
        .map(|part| {
            part.parse::<u64>()
                .unwrap_or_else(|error| panic!("{label}: {error}"))
        })
        .collect::<Vec<_>>();
    let [major, minor, patch] = parts.as_slice() else {
        panic!("{label}: three components");
    };
    (*major, *minor, *patch)
}

/// The desks [`REQUIRED_DESKS`] asks of `release`: every row whose floor is
/// at or below it.
fn required_desks_of(
    table: &'static [(&'static str, &'static str)],
    release: &str,
) -> Vec<&'static str> {
    table
        .iter()
        .filter(|(from, _)| release_triple(release) >= release_triple(from))
        .map(|(_, desk)| *desk)
        .collect()
}

/// EVERY RELEASE PINS EACH REQUIRED DESK FROM THAT DESK'S FLOOR ON, and every
/// one is checked in beside it. The cutter refuses a cut whose predecessor
/// misses one (`aterm-release`'s `gates::handoff_fixtures_of`, reading
/// [`REQUIRED_DESKS`] from this source); this is the same rule over the tree
/// as it stands, so a release whose fixtures miss a desk is red here before
/// anyone tries to cut after it.
#[test]
fn every_release_since_the_floor_pins_the_required_desks() {
    let desks = every_desk();
    let mut releases = desks
        .iter()
        .map(|desk| desk.release.as_str())
        .collect::<Vec<_>>();
    releases.dedup();
    for (from, desk) in REQUIRED_DESKS {
        assert!(
            releases.contains(from),
            "the floor {from} of {desk} names a release with fixtures"
        );
    }
    for release in releases {
        for required in required_desks_of(REQUIRED_DESKS, release) {
            assert!(
                PINNED_DESKS.contains(&(release, required)),
                "{release} pins the required desk {required}"
            );
            assert!(
                desks
                    .iter()
                    .any(|desk| desk.release == release && desk.name == required),
                "{release}/{required} is checked in"
            );
        }
    }
}

/// A DESK ADDED FOR A LATER RELEASE ASKS NOTHING OF AN EARLIER ONE (the
/// round-four review). The next release's `split-sequences` desk, required
/// from v0.98.0 on, is required of v0.98.0 and after and of no frozen release
/// before it, while every v0.97.0 desk stays required of v0.97.0.
///
/// FAILS WITHOUT THE FIX: the table had one floor for the whole set, so the
/// new desk was required of v0.97.0 too — whose fixtures can never gain it —
/// and the floor test above, and the cut's guard gate with it, went red.
#[test]
fn a_desk_added_for_a_later_release_asks_nothing_of_an_earlier_one() {
    const GROWN: &[(&str, &str)] = &[
        ("v0.97.0", "history"),
        ("v0.97.0", "stalled-sequence"),
        ("v0.98.0", "split-sequences"),
    ];
    assert_eq!(
        required_desks_of(GROWN, "v0.97.0"),
        ["history", "stalled-sequence"]
    );
    assert_eq!(
        required_desks_of(GROWN, "v0.98.0"),
        ["history", "stalled-sequence", "split-sequences"]
    );
    assert_eq!(
        required_desks_of(GROWN, "v0.100.0"),
        ["history", "stalled-sequence", "split-sequences"],
        "releases order as numbers, never as strings"
    );
    assert!(required_desks_of(GROWN, "v0.95.0").is_empty());
}

/// THE SUCCESSOR SAYS WHAT DEGRADED (the 2026-09-22/23 update audit, plan
/// P1-5). The v0.91.0 incident desk with its shell's meta rewritten to hold a
/// NUL in the directory — what an older producer carried for an OSC 7 `%00` —
/// still adopts both sessions, and the count the landing record reads
/// (`IncomingHandoff::repainted_tabs`) is the one session this build
/// repainted, which the landing record then names.
/// Before, the degrade was a log line only: the user found a blank tab with
/// no word about why.
#[test]
fn a_repainted_session_is_counted_for_the_landing_row() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let desk = every_desk()
        .into_iter()
        .find(|desk| desk.release == "v0.91.0" && desk.name == "incident-55x149")
        .expect("the incident fixture");
    let nul_cwd = |meta: &str| {
        let mut parsed: CheckpointMeta = aterm_json::from_str(meta).expect("fixture meta");
        parsed.current_working_directory = Some("/home/dev/a\0b".to_string());
        aterm_json::to_string(&parsed).expect("rewritten meta")
    };
    let staged = stage(&desk, Some((1, &nul_cwd)));
    let incoming = take_incoming_as(ReceiverShape::Current);
    assert_eq!(incoming.adopted.len(), 2, "both shells adopt");
    assert_eq!(incoming.repainted_tabs(), 1, "one of them was repainted");
    let words = crate::update_words::landed("0.92.0", 7, incoming.repainted_tabs(), None);
    assert_eq!(
        words.detail.first().map(String::as_str),
        Some("1 tab repainted"),
        "the landing record names the repaint"
    );
    staged.teardown();
}

// ------------------------------------------------- the rendezvous transport
//
// macOS-only, as `crate::handoff_rendezvous` is (lib.rs): the launched lane and
// the PTY-device proof term exist only there, so every item of this section is
// `#[cfg(target_os = "macos")]` and the other unix cells compile none of it.

/// Every release whose RENDEZVOUS TRANSPORT is pinned: `v<release>/rendezvous.toml`,
/// written by that release's own claim, grant and proof code in a worktree at
/// its tag (the fixture README says how). One literal `"vX.Y.0"` row per
/// release, like [`PINNED_DESKS`], so a lost file is a red test rather than a
/// guard that silently checks less.
#[cfg(target_os = "macos")]
const PINNED_RENDEZVOUS: &[&str] = &[
    "v0.91.0", "v0.92.0", "v0.93.0", "v0.94.0", "v0.95.0", "v0.97.0", "v0.98.0",
];

/// The fixed inputs every rendezvous capture runs over, in every release and in
/// this build: what a record holds is then the producer's SPELLING of them and
/// nothing else. Kept byte-identical to the generator's copy
/// (`tests/fixtures/handoff/generator.rs.txt`).
#[cfg(target_os = "macos")]
const RZ_SECRET: &str = "3f9c2e71a0b84d56e1f7c3a92b0d4e68f5a1c7e39b2d0f86a4e1c5b7d93f0a2c";
#[cfg(target_os = "macos")]
const RZ_NONCE: &str = "c0ffee00aa55aa55c0ffee00aa55aa55";
#[cfg(target_os = "macos")]
const RZ_SESSIONS: [(u64, i32); 3] = [(0, 4000), (1, 4001), (7, 4002)];

/// What one release's rendezvous transport really does (`rendezvous.toml`).
#[cfg(target_os = "macos")]
#[derive(serde::Deserialize)]
struct RendezvousRecord {
    producer_version: String,
    /// The fixed claim secret the generator dialed with ([`RZ_SECRET`]). Keyed
    /// `claim_input`, not `claim_secret`: the export's gitleaks scan reads
    /// `…secret = "<hex>"` as a leaked key (README, "The rendezvous transport").
    claim_input: String,
    /// The claim a successor presents for that secret, as that release's own
    /// dialer wrote it.
    claim_frame_hex: String,
    /// The grant that release's parent sends for [`RZ_NONCE`] and
    /// [`RZ_SESSIONS`]: the descriptor-carrying header, the body after it, and
    /// the order its descriptors arrived in (`session:<local_id>`, `ready`,
    /// `commit`), observed by a real receiver over a real `Rendezvous`.
    grant_nonce: String,
    grant_sessions: Vec<Vec<i64>>,
    grant_header_hex: String,
    grant_body: String,
    descriptor_order: Vec<String>,
    /// The launch environment's names, and the shape of their values.
    env_rendezvous: String,
    rendezvous_file_shape: String,
    env_claim: String,
    claim_hex_len: usize,
    env_proof_term: String,
    proof_term_launched: String,
    proof_term_fork: String,
    env_target: String,
    /// `ATERM_SEAMLESS_TARGET` as that release's parent spelled it for its own
    /// build and commit.
    target_value: String,
    env_parent_birth: String,
    /// `ATERM_HANDOFF_PARENT_BIRTH` as that release's parent published its own.
    parent_birth_value: String,
    /// The adoption proof over the PTY device terms (`st_rdev`) of that
    /// release's capture, with the target read from `target_value`.
    proof_layout_digest: String,
    proof_screen_digest: String,
    proof_terms: Vec<Vec<i64>>,
    proof_count: u32,
    proof_digest: String,
}

/// What ONE build's rendezvous code does with the fixed inputs.
#[cfg(target_os = "macos")]
struct TransportCapture {
    claim_frame: Vec<u8>,
    grant_header: Vec<u8>,
    grant_body: String,
    descriptor_order: Vec<String>,
    rendezvous_file_shape: String,
    claim_hex_len: usize,
}

/// Lowercase hex of `bytes`.
#[cfg(target_os = "macos")]
fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `fstat` of a live descriptor, or a panic naming it.
#[cfg(target_os = "macos")]
fn stat_of(fd: i32) -> libc::stat {
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fstat` writes one `struct stat` through the pointer.
    assert_eq!(
        unsafe { libc::fstat(fd, info.as_mut_ptr()) },
        0,
        "fstat {fd}"
    );
    // SAFETY: `fstat` returned 0, so it initialized the structure.
    unsafe { info.assume_init() }
}

/// The file a descriptor names: `(st_dev, st_ino)`.
#[cfg(target_os = "macos")]
fn inode_of(fd: i32) -> (libc::dev_t, libc::ino_t) {
    let info = stat_of(fd);
    (info.st_dev, info.st_ino)
}

/// A fresh PTY: `(master, slave)`.
#[cfg(target_os = "macos")]
fn open_pty_pair() -> (i32, i32) {
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
    assert_eq!(rc, 0, "openpty");
    (master, slave)
}

/// THIS BUILD'S TRANSPORT over the fixed inputs, observed from outside: the
/// claim its dialer writes (read off a plain listener), and the grant its
/// parent sends over a real bound [`crate::handoff_rendezvous::Rendezvous`] to
/// a real claim — the header and every descriptor dequeued by one
/// `recv_with_fds`, the body after it, and each descriptor identified by what
/// it IS (a master by its `st_rdev`, a pipe end by its inode). The generator
/// runs this same function in each release's worktree.
#[cfg(target_os = "macos")]
fn capture_transport(scratch: &std::path::Path) -> TransportCapture {
    use std::io::Read as _;
    use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _};

    let listen = scratch.join("claim.sock");
    let listener = std::os::unix::net::UnixListener::bind(&listen).expect("a plain listener");
    let dialer =
        crate::handoff_rendezvous::dial_for_test(&listen, RZ_SECRET).expect("dial and claim");
    let (mut accepted, _) = listener.accept().expect("accept the claim");
    drop(dialer);
    let mut claim_frame = Vec::new();
    accepted
        .read_to_end(&mut claim_frame)
        .expect("read the claim");
    drop((accepted, listener));
    let _ = std::fs::remove_file(&listen);

    let rendezvous =
        crate::handoff_rendezvous::Rendezvous::bind(RZ_NONCE).expect("bind a rendezvous");
    let file = rendezvous
        .path()
        .file_name()
        .expect("a socket name")
        .to_string_lossy()
        .into_owned();
    let rendezvous_file_shape = file
        .replace(&format!("-{}-", std::process::id()), "-<pid>-")
        .replace(&RZ_NONCE[..16], "<nonce16>");
    let claim_hex_len = rendezvous.claim().len();
    let dialer = crate::handoff_rendezvous::dial_for_test(rendezvous.path(), rendezvous.claim())
        .expect("dial the rendezvous");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let own = i32::try_from(std::process::id()).expect("a pid fits pid_t");
    let peer = rendezvous
        .accept_claim(Some(own), deadline, &|| false)
        .expect("the claim is accepted");
    let ptys = RZ_SESSIONS
        .iter()
        .map(|_| open_pty_pair())
        .collect::<Vec<_>>();
    let (ready_read, ready_write) = pipe_pair("rz ready");
    let (commit_read, commit_write) = pipe_pair("rz commit");
    // SAFETY: fresh pipe descriptors, owned from here.
    let (ready_write, commit_read) = unsafe {
        (
            std::os::fd::OwnedFd::from_raw_fd(ready_write),
            std::os::fd::OwnedFd::from_raw_fd(commit_read),
        )
    };
    let sessions = RZ_SESSIONS
        .iter()
        .zip(&ptys)
        .map(|((local_id, pid), (master, _))| {
            // SAFETY: the master stays open until the end of this function.
            (*local_id, *pid, unsafe {
                std::os::fd::BorrowedFd::borrow_raw(*master)
            })
        })
        .collect::<Vec<_>>();
    peer.transfer(
        RZ_NONCE,
        &sessions,
        ready_write.as_fd(),
        commit_read.as_fd(),
        deadline,
    )
    .expect("the grant is sent");
    dialer
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .expect("a read bound");
    let mut header = [0u8; 8];
    let received =
        aterm_uds::fdpass::recv_with_fds(&dialer, &mut header, aterm_uds::fdpass::MAX_FDS)
            .expect("the header and its descriptors");
    if received.bytes < header.len() {
        (&dialer)
            .read_exact(&mut header[received.bytes..])
            .expect("the rest of the header");
    }
    let mut body = vec![0u8; usize::from(u16::from_be_bytes([header[6], header[7]]))];
    (&dialer).read_exact(&mut body).expect("the grant body");
    let descriptor_order = received
        .fds
        .iter()
        .map(|fd| {
            let fd = fd.as_raw_fd();
            if stat_of(fd).st_mode & libc::S_IFMT == libc::S_IFCHR {
                RZ_SESSIONS
                    .iter()
                    .zip(&ptys)
                    .find(|(_, (master, _))| {
                        crate::handoff_rendezvous::pty_device_term(*master)
                            == crate::handoff_rendezvous::pty_device_term(fd)
                    })
                    .map_or_else(
                        || "unknown-device".to_string(),
                        |((local_id, _), _)| format!("session:{local_id}"),
                    )
            } else if inode_of(fd) == inode_of(ready_write.as_raw_fd()) {
                "ready".to_string()
            } else if inode_of(fd) == inode_of(commit_read.as_raw_fd()) {
                "commit".to_string()
            } else {
                "unknown".to_string()
            }
        })
        .collect::<Vec<_>>();
    drop(received);
    drop((dialer, peer, rendezvous, ready_write, commit_read));
    for fd in ptys
        .into_iter()
        .flat_map(|(master, slave)| [master, slave])
        .chain([ready_read, commit_write])
    {
        aterm_pty::close_fd(fd);
    }
    TransportCapture {
        claim_frame,
        grant_header: header.to_vec(),
        grant_body: String::from_utf8(body).expect("a UTF-8 grant body"),
        descriptor_order,
        rendezvous_file_shape,
        claim_hex_len,
    }
}

/// Every release's `rendezvous.toml`, by release.
#[cfg(target_os = "macos")]
fn every_rendezvous_record() -> Vec<(String, RendezvousRecord)> {
    let mut records = std::fs::read_dir(FIXTURES)
        .expect("the handoff fixture root exists")
        .flatten()
        .filter(|entry| entry.path().join("rendezvous.toml").is_file())
        .map(|entry| {
            let path = entry.path().join("rendezvous.toml");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let record: RendezvousRecord = aterm_toml::from_str(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            (entry.file_name().to_string_lossy().into_owned(), record)
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|(release, _)| release_triple(release));
    records
}

/// The local id of a recorded `[local_id, pid]` pair.
#[cfg(target_os = "macos")]
fn lid_of_entry(entry: &[i64]) -> u64 {
    u64::try_from(entry[0]).expect("a local id")
}

/// The stub parent: accept one dial on `listener`, check the claim it presents
/// is `expected_claim`, and send `header` carrying `fds`, then `body`. The
/// stream is handed back to be held until the successor has answered, as a real
/// parent holds it through the proof wait (Darwin refuses a socket option on a
/// stream whose peer has gone, and the successor sets one per read).
#[cfg(target_os = "macos")]
fn stub_parent_grant(
    listener: &std::os::unix::net::UnixListener,
    expected_claim: &[u8],
    header: &[u8],
    body: &[u8],
    fds: &[i32],
) -> Result<std::os::unix::net::UnixStream, String> {
    use std::io::{Read as _, Write as _};

    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > until {
                    return Err("no successor dialled".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => return Err(format!("accept: {error}")),
        }
    };
    stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_read_timeout(Some(std::time::Duration::from_secs(60))))
        .map_err(|error| error.to_string())?;
    let mut claim = vec![0u8; expected_claim.len()];
    (&stream)
        .read_exact(&mut claim)
        .map_err(|error| format!("read the claim: {error}"))?;
    if claim != expected_claim {
        return Err(format!(
            "the successor claimed {} where the parent expects {}",
            hex_of(&claim),
            hex_of(expected_claim)
        ));
    }
    let fds = fds
        .iter()
        // SAFETY: the caller keeps every descriptor open until this returns.
        .map(|fd| unsafe { std::os::fd::BorrowedFd::borrow_raw(*fd) })
        .collect::<Vec<_>>();
    let wrote = aterm_uds::fdpass::send_with_fds(&stream, header, &fds)
        .map_err(|error| format!("send the header: {error}"))?;
    (&stream)
        .write_all(&header[wrote..])
        .and_then(|()| (&stream).write_all(body))
        .map_err(|error| format!("send the body: {error}"))?;
    Ok(stream)
}

/// THIS BUILD'S SUCCESSOR, dialled at a stub parent that replays one release's
/// recorded grant — its header and body, byte for byte, and fresh PTY masters
/// and pipe ends in the order that release's parent sent its descriptors. The
/// stub also checks the claim this build presents against the one that
/// release's parent expects.
#[cfg(target_os = "macos")]
fn replay_grant_to_this_successor(
    label: &str,
    record: &RendezvousRecord,
    scratch: &std::path::Path,
) {
    use std::os::fd::AsRawFd as _;

    let socket = scratch.join("rz-parent.sock");
    let _ = std::fs::remove_file(&socket);
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("a stub parent");
    listener.set_nonblocking(true).expect("a pollable listener");
    let ptys = record
        .grant_sessions
        .iter()
        .map(|_| open_pty_pair())
        .collect::<Vec<_>>();
    let (ready_read, ready_write) = pipe_pair("replay ready");
    let (commit_read, commit_write) = pipe_pair("replay commit");
    let sent = record
        .descriptor_order
        .iter()
        .map(|slot| match slot.as_str() {
            "ready" => ready_write,
            "commit" => commit_read,
            other => {
                let local_id = other
                    .strip_prefix("session:")
                    .and_then(|id| id.parse::<u64>().ok())
                    .unwrap_or_else(|| panic!("{label}: descriptor slot {other:?}"));
                let index = record
                    .grant_sessions
                    .iter()
                    .position(|entry| lid_of_entry(entry) == local_id)
                    .unwrap_or_else(|| panic!("{label}: {other} is a granted session"));
                ptys[index].0
            }
        })
        .collect::<Vec<_>>();
    let header = unhex::<8>(label, &record.grant_header_hex);
    let claim = unhex::<70>(label, &record.claim_frame_hex);
    let now = std::time::Instant::now();
    let (stubbed, claimed) = std::thread::scope(|scope| {
        let stub = scope.spawn(|| {
            stub_parent_grant(
                &listener,
                &claim,
                &header,
                record.grant_body.as_bytes(),
                &sent,
            )
        });
        let claimed = crate::handoff_rendezvous::dial_and_claim_for_fixture(
            &socket,
            &record.claim_input,
            &record.grant_nonce,
            crate::handoff_rendezvous::ClaimDeadlines {
                dial: now + std::time::Duration::from_secs(30),
                grant: now + std::time::Duration::from_secs(60),
            },
        );
        (stub.join().expect("the stub parent"), claimed)
    });
    let held = stubbed.unwrap_or_else(|error| {
        panic!("{label}: the stub parent saw the claim that release's parent expects: {error}")
    });
    let (wire, masters, ready, commit) = claimed.unwrap_or_else(|error| {
        panic!("{label}: this build's successor claims that release's grant: {error}")
    });
    assert_eq!(
        masters.len(),
        record.grant_sessions.len(),
        "{label}: every master arrives"
    );
    let entries = wire.split(',').collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        record.grant_sessions.len(),
        "{label}: the wire {wire:?} names every session"
    );
    for (entry, (index, granted)) in entries.iter().zip(record.grant_sessions.iter().enumerate()) {
        let (local_id, rest) = entry.split_once('=').expect("<lid>=<fd>:<pid>");
        let (fd, pid) = rest.split_once(':').expect("<fd>:<pid>");
        let (local_id, fd, pid) = (
            local_id.parse::<u64>().expect("a local id"),
            fd.parse::<i32>().expect("an fd"),
            pid.parse::<i64>().expect("a pid"),
        );
        assert_eq!(
            (local_id, pid),
            (lid_of_entry(granted), granted[1]),
            "{label}: the wire pairs session {local_id} with its shell"
        );
        assert_eq!(
            crate::handoff_rendezvous::pty_device_term(fd),
            crate::handoff_rendezvous::pty_device_term(ptys[index].0),
            "{label}: session {local_id} is paired with the master its parent sent for it"
        );
    }
    assert_eq!(
        inode_of(ready.as_raw_fd()),
        inode_of(ready_write),
        "{label}: the readiness end is the one the parent sent"
    );
    assert_eq!(
        inode_of(commit.as_raw_fd()),
        inode_of(commit_read),
        "{label}: the Commit end is the one the parent sent"
    );
    drop((masters, ready, commit, held, listener));
    for fd in ptys
        .into_iter()
        .flat_map(|(master, slave)| [master, slave])
        .chain([ready_read, ready_write, commit_read, commit_write])
    {
        aterm_pty::close_fd(fd);
    }
    let _ = std::fs::remove_file(&socket);
}

/// THE RENDEZVOUS TRANSPORT IS THE SHIPPED ONE (item 11 of the fifth
/// update-robustness round). The desk guard above pins the manifests and
/// sidecars, but on macOS the successor gets every descriptor, and the parent
/// its claim, over the rendezvous — whose frames nothing pinned, so a change to
/// a magic, the header, the body or the descriptor order passed every
/// same-build test and failed every update in the field. For every release:
///
/// - this build's claim for the recorded secret is the claim that release's
///   parent expects, and this build's grant for the recorded nonce and
///   sessions is the one that release's successor reads — header, body and
///   descriptor order — so a grant of up to 62 sessions keeps its shape;
/// - this build's successor, dialled at a stub parent replaying that release's
///   recorded grant, pairs every session with the very master sent for it, and
///   takes the readiness and Commit ends it was sent;
/// - the launch environment's names are that release's, and the values that
///   release's parent published — its target, its birth record, the proof-term
///   spellings — parse here, from the recorded strings;
/// - the adoption proof over that release's recorded PTY device terms is its
///   own recorded proof.
#[cfg(target_os = "macos")]
#[test]
fn every_shipped_producers_rendezvous_grants_this_successor() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let records = every_rendezvous_record();
    let releases = records
        .iter()
        .map(|(release, _)| release.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        releases, PINNED_RENDEZVOUS,
        "every pinned release's rendezvous.toml is checked in, and every checked-in one is pinned"
    );
    let scratch = std::env::temp_dir().join(format!("aterm-rz-fixture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    let this = capture_transport(&scratch);
    for (release, record) in &records {
        let label = format!("{release}/rendezvous");
        assert_eq!(
            format!("v{}", record.producer_version),
            *release,
            "{label}: filed under the release that wrote it"
        );
        assert_eq!(
            (record.claim_input.as_str(), record.grant_nonce.as_str()),
            (RZ_SECRET, RZ_NONCE),
            "{label}: recorded over the fixed inputs"
        );
        assert_eq!(
            record
                .grant_sessions
                .iter()
                .map(|entry| (lid_of_entry(entry), i32::try_from(entry[1]).expect("pid")))
                .collect::<Vec<_>>(),
            RZ_SESSIONS,
            "{label}: recorded over the fixed sessions"
        );
        assert_eq!(
            hex_of(&this.claim_frame),
            record.claim_frame_hex,
            "{label}: this build's claim is the one that release's parent expects"
        );
        assert_eq!(
            hex_of(&this.grant_header),
            record.grant_header_hex,
            "{label}: this build's grant header is the one that release's successor reads"
        );
        assert_eq!(
            this.grant_body, record.grant_body,
            "{label}: this build's grant body is the one that release's successor reads"
        );
        assert_eq!(
            this.descriptor_order, record.descriptor_order,
            "{label}: this build sends its descriptors in that release's order"
        );
        assert_eq!(
            (this.rendezvous_file_shape.as_str(), this.claim_hex_len),
            (record.rendezvous_file_shape.as_str(), record.claim_hex_len),
            "{label}: the rendezvous socket and the claim secret keep their shapes"
        );
        assert_eq!(
            (
                record.env_rendezvous.as_str(),
                record.env_claim.as_str(),
                record.env_proof_term.as_str(),
                record.env_target.as_str(),
                record.env_parent_birth.as_str(),
            ),
            (
                crate::handoff_rendezvous::ENV_RENDEZVOUS,
                crate::handoff_rendezvous::ENV_CLAIM,
                crate::handoff_rendezvous::ENV_PROOF_TERM,
                ENV_TARGET,
                ENV_PARENT_BIRTH,
            ),
            "{label}: the launch environment's names"
        );
        assert_eq!(
            (
                crate::handoff_rendezvous::parse_proof_term(&record.proof_term_launched),
                crate::handoff_rendezvous::parse_proof_term(&record.proof_term_fork),
            ),
            (Some(true), Some(false)),
            "{label}: the proof-term spellings that release publishes read here"
        );
        let target = parse_target_identity(&record.target_value).unwrap_or_else(|| {
            panic!(
                "{label}: that release's target {:?} parses here",
                record.target_value
            )
        });
        assert!(
            ProcessBirth::from_wire(&record.parent_birth_value).is_some(),
            "{label}: that release's birth record {:?} parses here",
            record.parent_birth_value
        );
        let terms = record
            .proof_terms
            .iter()
            .map(|term| {
                let [local_id, rdev, pid] = term.as_slice() else {
                    panic!("{label}: a (local_id, st_rdev, pid) term");
                };
                (
                    u64::try_from(*local_id).expect("local id"),
                    i32::try_from(*rdev).expect("st_rdev"),
                    i32::try_from(*pid).expect("pid"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            adoption_proof(
                &record.grant_nonce,
                target.build,
                &target.commit,
                &unhex::<32>(&label, &record.proof_layout_digest),
                &unhex::<32>(&label, &record.proof_screen_digest),
                &terms,
            ),
            Some(AdoptionProof {
                count: record.proof_count,
                digest: unhex::<32>(&label, &record.proof_digest),
            }),
            "{label}: this build's proof over that release's device terms is its own"
        );
        replay_grant_to_this_successor(&label, record, &scratch);
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
