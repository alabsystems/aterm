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
//! consumer adopts EVERY session exactly (no repaint, no lost control carry, the
//! layout placed) and commits to the SAME digests the parent recorded, so the
//! older parent's adoption proof matches — and, for a release that carries
//! history (v0.94.0 on), that it takes every `.hist` sidecar before its proof
//! and, after Commit, imports every line of it back in front of the history
//! the checkpoint restored. Adoption alone could not see a broken import: the
//! handoff completes, and the tab just comes back with a screen and a half of
//! its history.
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
    ("v0.97.0", "shell-integration"),
    ("v0.97.0", "stalled-sequence"),
    ("v0.97.0", "twelve-panes"),
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
    let layout = String::from_utf8(desk.read(&format!(
        "{}.layout.toml",
        desk.parent.manifest.trim_end_matches(".toml")
    )))
    .expect("UTF-8 layout");
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
/// and unlinked, nothing is counted lost at this hop, and the session's
/// running loss from earlier handoffs crosses; so does the foreground holder
/// the park stamped. A release before the carry names no sidecar: nothing is
/// taken and nothing is counted.
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
        (0, expected.history_lost.unwrap_or(0)),
        "{label}: session {id} loses nothing at this hop, and its running loss crosses"
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
fn assert_history_imports(desk: &Desk, sessions: Vec<Adopted>) {
    let label = desk.label();
    for mut session in sessions {
        let id = session.local_id;
        let take = desk.expected(id).history_take.unwrap_or(0);
        let checkpoint = session
            .checkpoint
            .take()
            .unwrap_or_else(|| panic!("{label}: session {id} has its screen"));
        let mut history = std::mem::take(&mut session.history);
        let engine = Arc::new(Mutex::new(crate::spawn::new_live_terminal(
            checkpoint.rows,
            checkpoint.cols,
            None,
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
            (take, 0),
            "{label}: session {id} imports every line its parent carried, and loses none"
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
    }
}

/// THE GUARD. Every desk of every shipped release adopts in THIS build
/// exactly: every session, none repainted, each screen the carried bytes
/// verbatim at the carried geometry, each control carry read, the layout
/// placed — and both digests, and so the adoption proof, equal what that
/// release's parent committed to. Every history sidecar the parent named is
/// taken before the proof ([`assert_history_taken`]) and imported whole
/// after it ([`assert_history_imports`]).
#[test]
fn every_shipped_producers_desk_adopts_exactly_and_proves() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let desks = every_desk();
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
            0,
            "{label}: an honest desk of a shipped producer degrades nothing"
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
