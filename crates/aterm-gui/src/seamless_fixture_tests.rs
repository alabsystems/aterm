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
//! older parent's adoption proof matches.
//!
//! A red here is never fixed by regenerating a fixture: the bytes are what that
//! release's producer sends, forever. It is fixed by making the consumer admit
//! them again (law L3: consumer changes only ever become more lenient).

use std::sync::PoisonError;

use super::tests::{ENV_LOCK, RestoreVar, child_proof_from, pipe_pair};
use super::*;

/// The fixture root: one directory per release, one per desk inside it.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/handoff");

/// Every `(release, desk)` checked in — pinned so a lost fixture directory is
/// a red test rather than a guard that silently checks less. Each release
/// appends its own rows when it adds its directory (docs/RELEASING.md).
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
    live: Vec<SessionIdentity>,
    slaves: Vec<i32>,
    /// The parent's ends and the Commit channel's read end: always ours.
    pipes: [i32; 3],
    /// The readiness channel's write end, until the child's `take_ready_fd`
    /// takes it (its `ReadySignal` then owns and closes it).
    ready_write: Option<i32>,
}

impl Staged {
    /// Close every descriptor still ours — the masters too, which the
    /// adoption holds as plain numbers — and remove the scratch dir.
    fn teardown(self) {
        for fd in self
            .live
            .iter()
            .map(|(_, master, _)| *master)
            .chain(self.slaves)
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

/// THE GUARD. Every desk of every shipped release adopts in THIS build
/// exactly: every session, none repainted, each screen the carried bytes
/// verbatim at the carried geometry, each control carry read, the layout
/// placed — and both digests, and so the adoption proof, equal what that
/// release's parent committed to.
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
        }
        let ((proof, ready, adopted), _) = child_proof_from(incoming)
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
    let words = crate::update_words::landed("0.92.0", 7, incoming.repainted_tabs());
    assert_eq!(
        words.detail.first().map(String::as_str),
        Some("1 tab repainted"),
        "the landing record names the repaint"
    );
    staged.teardown();
}
