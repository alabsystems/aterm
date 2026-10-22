// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cross-version guard: the bytes a shipped producer really wrote, adopted by this build's
//! consumer.
//!
//! Every other handoff test writes and reads with this build, so a consumer change that refuses
//! what an older producer sends is green in the suite and red in the field, where the N to N+1
//! hop is decided by N's frozen producer. So each release's producer bytes are checked in under
//! `tests/fixtures/handoff/v<release>/<desk>/`, written by that release's own `write_outgoing`
//! through its generator (`seamless_fixture_generator_tests.rs`, run by its release cut; the
//! fixture README says how). For every release and desk this guard asserts that the current
//! consumer adopts every session exactly (no repaint but the ones its producer flagged, no lost
//! control carry, each meta's state reassembled, the layout placed with every Settings draft it
//! carries) and commits to the digests the parent recorded, so the older parent's proof matches;
//! and, from v0.94.0, that it takes every `.hist` sidecar before its proof and after Commit imports
//! every line of it in front of the history the checkpoint restored (adoption alone cannot see a
//! broken import).
//!
//! A red here is never fixed by regenerating a fixture: the bytes are what that
//! release's producer sends, forever. It is fixed by making the consumer admit
//! them again (law L3: consumer changes only ever become more lenient).
//!
//! ONE DIRECTION, the only one an update hands in: an older producer's bytes
//! into this consumer. No update authority names a build at or below the
//! running one (`native_updater_service`'s
//! `no_update_authority_names_a_build_at_or_below_the_running_one`), so no
//! lane hands this producer's bytes to an older consumer (the fixture
//! README's "The other direction"). What the desks cover is held by the
//! fields each release's desks write (`fields.toml`,
//! `seamless_fixture_cover.rs`), which the generator, and with it the cut,
//! keeps from shrinking.

use std::sync::{Arc, Mutex, PoisonError};

use aterm_core::scrollback::Line;
use aterm_core::terminal::Terminal;

use super::tests::{
    ScratchEnv, child_proof_from, env_alone, pipe_pair, restore_env, run_in_children,
};
use super::*;

/// The fixture root: one directory per release, one per desk inside it.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/handoff");

/// Every `(release, desk)` checked in, pinned so a lost fixture directory is a red test rather
/// than a guard that silently checks less. Each release appends its own rows when it adds its
/// directory (docs/RELEASING.md).
///
/// The release cutter reads this table from this source: `ship cut` refuses unless the release it
/// succeeds has a row here (`aterm-release`'s `gates::handoff_fixture_gate_at`). Keep it a literal
/// table of `("vX.Y.0", "<desk>")` pairs; a table the gate cannot read stops the cut.
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
    ("v0.100.0", "claude-code-1049"),
    ("v0.100.0", "colour-and-shell"),
    ("v0.100.0", "held-pane"),
    ("v0.100.0", "history"),
    ("v0.100.0", "history-carry"),
    ("v0.100.0", "incident-55x149"),
    ("v0.100.0", "link-dense"),
    ("v0.100.0", "link-dense-history"),
    ("v0.100.0", "policy-repaint"),
    ("v0.100.0", "policy-visible"),
    ("v0.100.0", "shell-integration"),
    ("v0.100.0", "stalled-sequence"),
    ("v0.100.0", "twelve-panes"),
    ("v0.101.0", "claude-code-1049"),
    ("v0.101.0", "colour-and-shell"),
    ("v0.101.0", "held-pane"),
    ("v0.101.0", "history"),
    ("v0.101.0", "history-carry"),
    ("v0.101.0", "incident-55x149"),
    ("v0.101.0", "link-dense"),
    ("v0.101.0", "link-dense-history"),
    ("v0.101.0", "policy-repaint"),
    ("v0.101.0", "policy-visible"),
    ("v0.101.0", "shell-integration"),
    ("v0.101.0", "stalled-sequence"),
    ("v0.101.0", "twelve-panes"),
    ("v0.102.0", "claude-code-1049"),
    ("v0.102.0", "colour-and-shell"),
    ("v0.102.0", "held-pane"),
    ("v0.102.0", "history"),
    ("v0.102.0", "history-carry"),
    ("v0.102.0", "incident-55x149"),
    ("v0.102.0", "link-dense"),
    ("v0.102.0", "link-dense-history"),
    ("v0.102.0", "policy-repaint"),
    ("v0.102.0", "policy-visible"),
    ("v0.102.0", "shell-integration"),
    ("v0.102.0", "stalled-sequence"),
    ("v0.102.0", "twelve-panes"),
    ("v0.103.0", "claude-code-1049"),
    ("v0.103.0", "colour-and-shell"),
    ("v0.103.0", "held-pane"),
    ("v0.103.0", "history"),
    ("v0.103.0", "history-carry"),
    ("v0.103.0", "incident-55x149"),
    ("v0.103.0", "link-dense"),
    ("v0.103.0", "link-dense-history"),
    ("v0.103.0", "policy-repaint"),
    ("v0.103.0", "policy-visible"),
    ("v0.103.0", "shell-integration"),
    ("v0.103.0", "stalled-sequence"),
    ("v0.103.0", "twelve-panes"),
    ("v0.104.0", "claude-code-1049"),
    ("v0.104.0", "colour-and-shell"),
    ("v0.104.0", "held-pane"),
    ("v0.104.0", "history"),
    ("v0.104.0", "history-carry"),
    ("v0.104.0", "incident-55x149"),
    ("v0.104.0", "link-dense"),
    ("v0.104.0", "link-dense-history"),
    ("v0.104.0", "policy-repaint"),
    ("v0.104.0", "policy-visible"),
    ("v0.104.0", "shell-integration"),
    ("v0.104.0", "stalled-sequence"),
    ("v0.104.0", "twelve-panes"),
    ("v0.105.0", "claude-code-1049"),
    ("v0.105.0", "colour-and-shell"),
    ("v0.105.0", "held-pane"),
    ("v0.105.0", "history"),
    ("v0.105.0", "history-carry"),
    ("v0.105.0", "incident-55x149"),
    ("v0.105.0", "link-dense"),
    ("v0.105.0", "link-dense-history"),
    ("v0.105.0", "policy-repaint"),
    ("v0.105.0", "policy-visible"),
    ("v0.105.0", "shell-integration"),
    ("v0.105.0", "stalled-sequence"),
    ("v0.105.0", "twelve-panes"),
    ("v0.106.0", "claude-code-1049"),
    ("v0.106.0", "colour-and-shell"),
    ("v0.106.0", "held-pane"),
    ("v0.106.0", "history"),
    ("v0.106.0", "history-carry"),
    ("v0.106.0", "incident-55x149"),
    ("v0.106.0", "link-dense"),
    ("v0.106.0", "link-dense-history"),
    ("v0.106.0", "policy-repaint"),
    ("v0.106.0", "policy-visible"),
    ("v0.106.0", "shell-integration"),
    ("v0.106.0", "stalled-sequence"),
    ("v0.106.0", "twelve-panes"),
    ("v0.107.0", "claude-code-1049"),
    ("v0.107.0", "colour-and-shell"),
    ("v0.107.0", "held-pane"),
    ("v0.107.0", "history"),
    ("v0.107.0", "history-carry"),
    ("v0.107.0", "incident-55x149"),
    ("v0.107.0", "link-dense"),
    ("v0.107.0", "link-dense-history"),
    ("v0.107.0", "policy-repaint"),
    ("v0.107.0", "policy-visible"),
    ("v0.107.0", "shell-integration"),
    ("v0.107.0", "stalled-sequence"),
    ("v0.107.0", "twelve-panes"),
    ("v0.108.0", "carried-state"),
    ("v0.108.0", "claude-code-1049"),
    ("v0.108.0", "colour-and-shell"),
    ("v0.108.0", "engine-state"),
    ("v0.108.0", "fold"),
    ("v0.108.0", "held-failed"),
    ("v0.108.0", "held-pane"),
    ("v0.108.0", "held-withheld"),
    ("v0.108.0", "history"),
    ("v0.108.0", "history-carry"),
    ("v0.108.0", "incident-55x149"),
    ("v0.108.0", "link-dense"),
    ("v0.108.0", "link-dense-history"),
    ("v0.108.0", "policy-repaint"),
    ("v0.108.0", "policy-visible"),
    ("v0.108.0", "shell-integration"),
    ("v0.108.0", "stalled-sequence"),
    ("v0.108.0", "twelve-panes"),
    ("v0.108.0", "variants"),
    ("v0.109.0", "carried-state"),
    ("v0.109.0", "claude-code-1049"),
    ("v0.109.0", "colour-and-shell"),
    ("v0.109.0", "engine-state"),
    ("v0.109.0", "fold"),
    ("v0.109.0", "held-failed"),
    ("v0.109.0", "held-pane"),
    ("v0.109.0", "held-withheld"),
    ("v0.109.0", "history"),
    ("v0.109.0", "history-carry"),
    ("v0.109.0", "incident-55x149"),
    ("v0.109.0", "link-dense"),
    ("v0.109.0", "link-dense-history"),
    ("v0.109.0", "policy-repaint"),
    ("v0.109.0", "policy-visible"),
    ("v0.109.0", "shell-integration"),
    ("v0.109.0", "stalled-sequence"),
    ("v0.109.0", "twelve-panes"),
    ("v0.109.0", "variants"),
    ("v0.110.0", "carried-state"),
    ("v0.110.0", "claude-code-1049"),
    ("v0.110.0", "colour-and-shell"),
    ("v0.110.0", "engine-state"),
    ("v0.110.0", "fold"),
    ("v0.110.0", "held-failed"),
    ("v0.110.0", "held-pane"),
    ("v0.110.0", "held-withheld"),
    ("v0.110.0", "history"),
    ("v0.110.0", "history-carry"),
    ("v0.110.0", "incident-55x149"),
    ("v0.110.0", "link-dense"),
    ("v0.110.0", "link-dense-history"),
    ("v0.110.0", "policy-repaint"),
    ("v0.110.0", "policy-visible"),
    ("v0.110.0", "shell-integration"),
    ("v0.110.0", "stalled-sequence"),
    ("v0.110.0", "twelve-panes"),
    ("v0.110.0", "variants"),
    ("v0.111.0", "carried-state"),
    ("v0.111.0", "claude-code-1049"),
    ("v0.111.0", "colour-and-shell"),
    ("v0.111.0", "engine-state"),
    ("v0.111.0", "fold"),
    ("v0.111.0", "held-failed"),
    ("v0.111.0", "held-pane"),
    ("v0.111.0", "held-withheld"),
    ("v0.111.0", "history"),
    ("v0.111.0", "history-carry"),
    ("v0.111.0", "incident-55x149"),
    ("v0.111.0", "link-dense"),
    ("v0.111.0", "link-dense-history"),
    ("v0.111.0", "policy-repaint"),
    ("v0.111.0", "policy-visible"),
    ("v0.111.0", "shell-integration"),
    ("v0.111.0", "stalled-sequence"),
    ("v0.111.0", "twelve-panes"),
    ("v0.111.0", "variants"),
    ("v0.112.0", "carried-state"),
    ("v0.112.0", "claude-code-1049"),
    ("v0.112.0", "colour-and-shell"),
    ("v0.112.0", "engine-state"),
    ("v0.112.0", "fold"),
    ("v0.112.0", "held-failed"),
    ("v0.112.0", "held-pane"),
    ("v0.112.0", "held-withheld"),
    ("v0.112.0", "history"),
    ("v0.112.0", "history-carry"),
    ("v0.112.0", "incident-55x149"),
    ("v0.112.0", "link-dense"),
    ("v0.112.0", "link-dense-history"),
    ("v0.112.0", "policy-repaint"),
    ("v0.112.0", "policy-visible"),
    ("v0.112.0", "shell-integration"),
    ("v0.112.0", "stalled-sequence"),
    ("v0.112.0", "twelve-panes"),
    ("v0.112.0", "variants"),
    ("v0.113.0", "carried-state"),
    ("v0.113.0", "claude-code-1049"),
    ("v0.113.0", "colour-and-shell"),
    ("v0.113.0", "engine-state"),
    ("v0.113.0", "fold"),
    ("v0.113.0", "held-failed"),
    ("v0.113.0", "held-pane"),
    ("v0.113.0", "held-withheld"),
    ("v0.113.0", "history"),
    ("v0.113.0", "history-carry"),
    ("v0.113.0", "incident-55x149"),
    ("v0.113.0", "link-dense"),
    ("v0.113.0", "link-dense-history"),
    ("v0.113.0", "policy-repaint"),
    ("v0.113.0", "policy-visible"),
    ("v0.113.0", "shell-integration"),
    ("v0.113.0", "stalled-sequence"),
    ("v0.113.0", "twelve-panes"),
    ("v0.113.0", "variants"),
];

/// The desk set, one `("vX.Y.0", "<desk>")` row per desk: every release from that floor on must
/// pin the desk. One desk per producer shape a shipped release changed, so a release cannot pass
/// the cutter with one desk and leave the rest of its producer unchecked.
///
/// The floor is per desk because releases are frozen: a desk for a shape release N changed is
/// required from N on, since the releases before it never wrote it and never can (v0.94.0's and
/// v0.95.0's producers never wrote `stalled-sequence`; the policy and link-stripping desks stand
/// for producer paths v0.97.0 added). One floor for the whole set would turn every earlier release
/// red when a desk is added, or drop those releases from the check.
///
/// The release cutter reads this table from this source too (`aterm-release`'s
/// `gates::handoff_fixtures_of`): keep it a literal table of `("vX.Y.0", "<desk>")` pairs.
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
    ("v0.100.0", "held-pane"),
];

/// Every held pane the producer carried (v0.100.0 on) is shown in the order it was named, at its
/// carried geometry, without scrollback, with how its command ended, and with no link left (the
/// successor strips every link from a held screen, which has no process behind it).
fn assert_held_panes_shown(desk: &Desk, incoming: &IncomingHandoff) {
    let label = desk.label();
    assert_eq!(
        incoming
            .held
            .iter()
            .map(|pane| pane.local_id)
            .collect::<Vec<_>>(),
        desk.parent
            .held
            .iter()
            .map(|pane| pane.local_id)
            .collect::<Vec<_>>(),
        "{label}: every held pane the producer carried is shown, and no other"
    );
    for (shown, expected) in incoming.held.iter().zip(&desk.parent.held) {
        let cp = &shown.checkpoint;
        assert_eq!(
            (cp.rows, cp.cols, cp.history_lines, cp.alt_grid.is_some()),
            (expected.rows, expected.cols, 0, expected.alt_grid),
            "{label}: held pane {} keeps its carried geometry, without scrollback",
            shown.local_id
        );
        let exit = match (expected.exit_code, expected.exit_signal) {
            (Some(code), None) => Some(aterm_pty::ChildExit::Code(code)),
            (None, Some(signal)) => Some(aterm_pty::ChildExit::Signal(signal)),
            _ => None,
        };
        assert_eq!(
            shown.exit, exit,
            "{label}: held pane {} says how its command ended",
            shown.local_id
        );
        for grid in [Some(&cp.grid), cp.alt_grid.as_ref()].into_iter().flatten() {
            assert!(
                !grid.windows(b"https://".len()).any(|w| w == b"https://"),
                "{label}: held pane {} shows no link",
                shown.local_id
            );
        }
        // What v0.101.0 on carry beside the screen; an older release's
        // `parent.toml` records none of it and is asked about none.
        if let Some(sid) = &expected.sid {
            assert_eq!(
                shown.sid.as_ref().map(SessionId::as_str),
                Some(sid.as_str()),
                "{label}: held pane {} keeps the session id it was known by",
                shown.local_id
            );
        }
        if let Some(take) = expected.history_take {
            assert_eq!(
                shown
                    .history
                    .carry
                    .as_ref()
                    .map(crate::handoff_history::HistoryCarry::take),
                Some(take).filter(|take| *take > 0),
                "{label}: held pane {}'s scrollback sidecar is taken, {take} line(s) deep",
                shown.local_id
            );
        }
        if let Some(dropped) = expected.history_dropped {
            assert_eq!(
                (shown.history.dropped, shown.history.lost),
                (dropped, dropped),
                "{label}: held pane {} loses nothing at this hop but what its parent counted",
                shown.local_id
            );
        }
        if let Some(control) = expected.control {
            assert_eq!(
                shown.control.is_some(),
                control,
                "{label}: held pane {}'s control carry crosses",
                shown.local_id
            );
        }
        if let Some(seen) = expected.failure_seen {
            assert_eq!(
                shown.failure_seen, seen,
                "{label}: held pane {} says whether its failure was seen",
                shown.local_id
            );
        }
    }
}

/// What one desk's producer committed to (`parent.toml`, written beside the
/// bytes by the release's generator).
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
    /// The panes kept open after their command exited (`--hold`) whose final screens the producer
    /// carried, from v0.100.0; an older producer carries none, and the successor shows each such
    /// pane as its placeholder.
    #[serde(default)]
    held: Vec<HeldExpectation>,
}

/// One held pane's screen as the producer carried it, and how its command
/// ended.
#[derive(serde::Deserialize)]
struct HeldExpectation {
    local_id: u64,
    rows: u16,
    cols: u16,
    alt_grid: bool,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    exit_signal: Option<u8>,
    /// What the producer carried beside the screen, from v0.101.0: the pane's session id, how many
    /// lines of scrollback its `.h<id>.hist` sidecar carries (`0` for none), whether its control
    /// carry crossed, and whether the person had seen its failure.
    #[serde(default)]
    sid: Option<String>,
    #[serde(default)]
    history_take: Option<u64>,
    /// The lines of its scrollback the producer counted as left behind
    /// (`HeldPaneCarry::history_dropped`), from v0.101.0.
    #[serde(default)]
    history_dropped: Option<u64>,
    #[serde(default)]
    control: Option<bool>,
    #[serde(default)]
    failure_seen: Option<bool>,
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
    /// The shell-integration posture, from v0.92.0: whether marks must be signed, and the nonce
    /// they are signed with, as 64 hex digits.
    #[serde(default)]
    require_shell_integration_nonce: Option<bool>,
    #[serde(default)]
    shell_integration_nonce: Option<String>,
    /// The history carry, from v0.94.0: how many lines older than the checkpoint's the session's
    /// `.s<id>.hist` sidecar carries (`0` when it names none), and the session's running count of
    /// lines earlier handoffs could not carry. Older producers name no sidecar and carry no count.
    #[serde(default)]
    history_take: Option<u64>,
    #[serde(default)]
    history_lost: Option<u64>,
    /// The foreground holder the park stamped on the record (v0.94.0 on; `0` for none), which
    /// seeds the adopted session's reader.
    #[serde(default)]
    fg_holder: Option<i32>,
    /// The meta v0.95.0 added: the absolute row counters the successor continues (so an absolute
    /// row names the same line after the handoff), whether an application colour diff rides the
    /// meta, and the shell-integration state (the OSC 133 phase, how many command marks cross,
    /// the completed-command count). [`assert_meta_state_crosses`] reads them.
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
    /// The ladder's lower rungs, from v0.97.0: whether the producer flagged the session for a
    /// repaint (its screen carried blank, as a `carry = "repaint"` policy asks), and how many lines
    /// of its history this hop left behind, counted. Older desks repaint and drop nothing.
    /// (`rung` is recorded for review only: a consumer sees only these two.)
    #[serde(default)]
    repaint: Option<bool>,
    #[serde(default)]
    history_dropped: Option<u64>,
    /// The meta and record v0.101.0 added: whether the meta carries the engine's in-flight output
    /// state (the REP character every session that printed has, an open link, the XTPUSHSGR
    /// stack...), and the rows of render debt the record carries for a carried alternate screen
    /// (`"<shift>,<pushed>"`, absent for none; its columns are the record's own,
    /// [`carried_render_debt_cuts`]). Older producers wrote neither, and every such session adopts
    /// owing nothing.
    #[serde(default)]
    output_state: Option<bool>,
    #[serde(default)]
    render_debt: Option<String>,
    /// Columns recorded by newer generators. Earlier parent summaries omitted this
    /// additive key even where their immutable manifest carried it; the raw wire is
    /// checked independently below, and a recorded count must agree with it.
    #[serde(default)]
    render_debt_cut: Option<u64>,
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

    /// The name of held pane `local_id`'s history sidecar beside the
    /// manifest (v0.101.0 on).
    fn held_hist_name(&self, local_id: u64) -> String {
        format!(
            "{}.h{local_id}.hist",
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
    // In release order, not the names': `v0.100.0` sorts before `v0.91.0` as text, and the
    // controls below take the newest release's desk (`rfind`).
    releases.sort_by_key(|entry| release_triple(&entry.file_name().to_string_lossy()));
    releases
        .iter()
        .flat_map(|release| release_desks(&release.file_name().to_string_lossy(), &release.path()))
        .collect()
}

/// The desks of `release` in `dir`, one per directory holding a `parent.toml`, in name order.
fn release_desks(release: &str, dir: &std::path::Path) -> Vec<Desk> {
    let mut names = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: a release fixture dir: {error}", dir.display()))
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    names.sort_by_key(std::fs::DirEntry::file_name);
    names
        .into_iter()
        .map(|desk| {
            let path = desk.path();
            let text = std::fs::read_to_string(path.join("parent.toml"))
                .unwrap_or_else(|error| panic!("{}: parent.toml: {error}", path.display()));
            let parent: ParentRecord = aterm_toml::from_str(&text)
                .unwrap_or_else(|error| panic!("{}: parent.toml: {error}", path.display()));
            Desk {
                release: release.to_string(),
                name: desk.file_name().to_string_lossy().into_owned(),
                path,
                parent,
            }
        })
        .collect()
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
    // The history carry (v0.94.0 on): each record's stamp, `"<len> <sha256hex> <take>"`, names the
    // depth `parent.toml` recorded, and the sidecar is exactly the bytes it stamps. Parsed by
    // hand, not by the consumer's `parse_stamp`: this function involves no consumer.
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

/// The first `take` lines of a history sidecar, read off the bytes the parent wrote in v0.94.0's
/// format (`ATHIST1`: an eight-byte magic, the wrap width as `u16` LE, two zero bytes, then frames
/// of a `u32` LE length and a line-codec block). Decoded by the line codec alone, not the
/// consumer's reader: the import is what this guard checks.
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

/// `got` is `want`, line for line: as text (so a failure reads as the lines that differ), then as
/// each line's line-codec encoding, so an import that keeps the words and drops the colours fails.
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

/// The proof function and its frames are the parent's: the producer recorded the proof its own
/// `adoption_proof` took over fixed inputs and the `ProofReady`/`Commit` frames for it, and this
/// build's must be the same bytes (domain, field order, `READY_WIRE_LEN`, both magics), or every
/// parent of that release reads `AdoptionMismatch` whatever the consumer admits.
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

/// A desk laid out for this process to adopt as a successor: the release's files copied into a
/// private control dir (the consumer deletes what it takes), fresh PTYs standing in for the
/// parent's, and the launch environment `outgoing_parent_env` and the worker would publish.
struct Staged {
    /// The scratch HOME and runtime dir, put back and removed by `teardown`.
    scratch: ScratchEnv,
    /// The private control dir the desk's files were staged into, where the consumer takes (and
    /// unlinks) each sidecar.
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
    /// Close every descriptor still ours, put HOME and the runtime dir back and remove the
    /// scratch. The masters are the successor's once the wire names them: the adoption closes
    /// each as it drops, and a refusal closes them at once.
    fn teardown(self) {
        for fd in self
            .slaves
            .into_iter()
            .chain(self.pipes)
            .chain(self.ready_write)
        {
            aterm_pty::close_fd(fd);
        }
        drop(self.scratch);
    }
}

/// One session's meta rewrite: the local id and what its JSON becomes.
type MetaRewrite<'a> = (u64, &'a dyn Fn(&str) -> String);

/// Stage `desk`, with `rewrite_meta` applied to the manifest's copy of the named session's meta,
/// standing in for a producer this build disagrees with (the recorded digests then no longer
/// describe the wire).
fn stage(desk: &Desk, rewrite_meta: Option<MetaRewrite<'_>>) -> Staged {
    stage_with(desk, rewrite_meta, None)
}

/// [`stage`], with `rewrite_manifest` applied to the manifest's record keys (no digest covers
/// them), standing in for a producer that wrote a key this desk's did not.
fn stage_with(
    desk: &Desk,
    rewrite_meta: Option<MetaRewrite<'_>>,
    rewrite_manifest: Option<&dyn Fn(&mut SessionHandoff)>,
) -> Staged {
    // Every caller holds ENV_LOCK for its whole body.
    let scratch = ScratchEnv::home(&format!("{}-{}", desk.release, desk.name));
    let dir = crate::control_auth::socket_dir().expect("scratch control dir");
    let dir_text = dir.to_str().expect("a UTF-8 scratch dir");
    for file in &desk.parent.files {
        let bytes = desk.read(file);
        let bytes = if *file == desk.parent.manifest {
            // The manifest names each sidecar by absolute path in the parent's private dir,
            // which the generator swapped for a placeholder. No digest covers the manifest's
            // bytes, only the meta strings inside it, which the substitution cannot reach
            // (`assert_fixture_is_self_consistent`).
            let text = String::from_utf8(bytes)
                .expect("UTF-8 manifest")
                .replace(&desk.parent.dir_placeholder, dir_text);
            if rewrite_meta.is_none() && rewrite_manifest.is_none() {
                text.into_bytes()
            } else {
                let (nonce, toml) = text.split_once('\n').expect("nonce header");
                let mut manifest = SessionHandoff::from_toml(toml).expect("manifest");
                if let Some((local_id, rewrite)) = rewrite_meta {
                    let carry = manifest
                        .sessions
                        .iter_mut()
                        .find(|rec| rec.local_id == local_id)
                        .and_then(|rec| rec.screen.as_mut())
                        .expect("the rewritten session carries a screen");
                    carry.meta = rewrite(&carry.meta);
                }
                if let Some(rewrite) = rewrite_manifest {
                    rewrite(&mut manifest);
                }
                format!("{nonce}\n{}", manifest.to_toml().expect("reserialize")).into_bytes()
            }
        } else {
            bytes
        };
        std::fs::write(dir.join(file), bytes).expect("stage a fixture file");
    }
    let mut live = Vec::new();
    let mut slaves = Vec::new();
    for (index, session) in desk.parent.session.iter().enumerate() {
        let (master, slave) = aterm_pty::open_pty_pair()
            .unwrap_or_else(|error| panic!("open_pty_pair {index}: {error}"));
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
            crate::build_info::build_number()
                .parse::<u64>()
                .unwrap_or(0),
            crate::build_info::git_commit(),
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

/// The history carry is taken before the proof (v0.94.0 on): the sidecar the parent named for
/// `adopted` is opened whole, at the depth it stamped, and unlinked; nothing is counted lost at
/// this hop but the lines the parent left behind and counted (`history_dropped`, v0.97.0 on); and
/// the session's running loss and the foreground holder cross.
///
/// Checked before the proof, not only through the import after it: the outgoing process retires
/// every sidecar still in its dir once the proof checks out, so a sidecar not opened before the
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

/// The history lands after Commit (v0.94.0 on): each adopted session is hydrated as
/// `spawn_session` hydrates it and imported as the successor's worker imports it. Every stamped
/// line is imported, none lost but what the parent counted, and the history is then the parent's
/// sidecar lines in front of the lines the checkpoint restored, cell for cell; a session that
/// names no sidecar keeps what its checkpoint restored. The meta's state lands too (v0.95.0 on,
/// [`assert_meta_state_lands`]), on an engine that allows palette changes as the
/// `colour-and-shell` desk's parent did.
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

/// A held pane's scrollback lands after Commit (v0.101.0 on): each shown pane is restored as
/// `App::restore_held_leaf` restores it ([`crate::held_pane_session`], then
/// [`crate::held_pane_history`]) and imported by the shared worker. Every stamped line is
/// imported, none lost but what the parent counted, and the pane's history is then the parent's
/// sidecar lines with every link dropped, cell for cell. [`assert_held_panes_shown`] sees only
/// the sidecar's stamp, so this is what catches a broken held import.
fn assert_held_history_imports(desk: &Desk, held: Vec<crate::seamless::HeldPane>) {
    let label = desk.label();
    for pane in held {
        let id = pane.local_id;
        let expected = desk
            .parent
            .held
            .iter()
            .find(|expected| expected.local_id == id)
            .unwrap_or_else(|| panic!("{label}: parent.toml has held pane {id}"));
        let take = expected.history_take.unwrap_or(0);
        let dropped = expected.history_dropped.unwrap_or(0);
        let session = crate::held_pane_session(
            id,
            pane.sid.unwrap_or_else(SessionId::generate),
            &pane.checkpoint,
            None,
        );
        let Some(history) = crate::held_pane_history(&session.term, pane.history) else {
            assert_eq!(
                (take, dropped),
                (0, 0),
                "{label}: held pane {id} owes the import nothing only when its parent carried \
                 and counted no scrollback"
            );
            continue;
        };
        assert_eq!(
            history.claim.is_some(),
            take > 0,
            "{label}: held pane {id}'s import keys are reserved in front of its restored screen"
        );
        let restored = history_lines(
            &label,
            &session.term.lock().unwrap_or_else(PoisonError::into_inner),
        );
        assert!(
            restored.is_empty(),
            "{label}: held pane {id}'s screen restores no scrollback of its own"
        );
        let report = crate::handoff_history::run_imports(vec![crate::handoff_history::ImportJob {
            session: id,
            term: Arc::clone(&session.term),
            history,
        }])
        .remove(0);
        assert_eq!(
            (
                report.failed.as_deref(),
                report.failed_lines,
                report.cleared
            ),
            (None, 0, false),
            "{label}: held pane {id}'s import settles"
        );
        assert_eq!(
            (report.imported, report.lost(), report.held_pane),
            (take, dropped, true),
            "{label}: held pane {id} imports every line its parent carried, and loses none but \
             what its parent counted"
        );
        let mut want = if take > 0 {
            sidecar_history(&label, &desk.read(&desk.held_hist_name(id)), take)
        } else {
            Vec::new()
        };
        for line in &mut want {
            line.clear_hyperlinks();
        }
        let got = history_lines(
            &label,
            &session.term.lock().unwrap_or_else(PoisonError::into_inner),
        );
        assert_same_lines(
            &label,
            &got,
            &want,
            &format!("held pane {id}'s history is its parent's sidecar, every link dropped"),
        );
        assert!(
            !got.iter().any(Line::has_hyperlinks),
            "{label}: held pane {id}'s scrollback shows no link"
        );
    }
}

/// The text of absolute row `abs` of `terminal`'s history-holding grid, history or visible; `None`
/// once the row is evicted or past the screen.
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

/// One block a full clear blanked, as `(id, text_cleared, command_text_cleared,
/// prompt_start_row, command_start_row, output_start_row)`.
type BlankedBlock = (u64, bool, bool, u64, Option<u64>, Option<u64>);

/// The blocks a full clear blanked in session `local_id`'s parent, read off the producer's own
/// meta bytes (`s<id>.meta.json`, which [`assert_fixture_is_self_consistent`] holds to the
/// manifest) through `aterm_json`'s value tree, never this build's decode of them, which is what
/// is checked: every finished block (`shell.output_blocks`) and the block being built
/// (`shell.current_block`) that carries `text_cleared` or `command_text_cleared`, in the meta's
/// order. A meta that carries neither key blanked none (every desk before v0.108.0's
/// `engine-state`, whose shell ran `clear`).
fn carried_blanked_blocks(desk: &Desk, local_id: u64) -> Vec<BlankedBlock> {
    use aterm_json::Value;
    let label = desk.label();
    let meta = aterm_json::from_slice::<Value>(&desk.read(&format!("s{local_id}.meta.json")))
        .unwrap_or_else(|error| panic!("{label}: session {local_id}'s meta is JSON: {error}"));
    let Some(shell) = meta.get("shell") else {
        return Vec::new();
    };
    let flag = |block: &Value, key: &str| {
        block.get(key).is_some_and(|flag| {
            flag.as_bool().unwrap_or_else(|| {
                panic!("{label}: session {local_id}'s block {key} is a bool: {flag:?}")
            })
        })
    };
    let row = |span: &Value, key: &str| match span.get(key) {
        None | Some(Value::Null) => None,
        Some(row) => Some(row.as_u64().unwrap_or_else(|| {
            panic!("{label}: session {local_id}'s block {key} is a row: {row:?}")
        })),
    };
    shell
        .get("output_blocks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(shell.get("current_block").filter(|block| !block.is_null()))
        .filter_map(|block| {
            let (text, command) = (
                flag(block, "text_cleared"),
                flag(block, "command_text_cleared"),
            );
            if !(text || command) {
                return None;
            }
            let id = block.get("id").and_then(Value::as_u64);
            let span = block.get("span");
            let prompt = span.and_then(|span| row(span, "prompt_start_row"));
            let (Some(id), Some(span), Some(prompt)) = (id, span, prompt) else {
                panic!(
                    "{label}: session {local_id}'s blanked block names its id and rows: {block:?}"
                )
            };
            Some((
                id,
                text,
                command,
                prompt,
                row(span, "command_start_row"),
                row(span, "output_start_row"),
            ))
        })
        .collect()
}

/// The full-clear flags cross the decode: the blocks this build's reader of session `local_id`'s
/// meta (`checkpoint`, the adopted session's) carries as blanked are exactly the ones the
/// producer's bytes carry ([`carried_blanked_blocks`]), at the same rows. [`assert_meta_state_lands`]
/// then takes which marks a clear blanked from that checkpoint, so a reader that lost a flag (a
/// rename, a serde default) fails here, by name, and not later on the blank prompt row the clear
/// left, which would blame the marks' numbering.
fn assert_clear_flags_decoded(
    desk: &Desk,
    local_id: u64,
    checkpoint: &aterm_core::terminal::TerminalCheckpoint,
) {
    let decoded = checkpoint
        .shell
        .output_blocks
        .iter()
        .chain(&checkpoint.shell.current_block)
        .filter(|block| block.text_cleared || block.command_text_cleared)
        .map(|block| {
            (
                block.id,
                block.text_cleared,
                block.command_text_cleared,
                block.span.prompt_start_row,
                block.span.command_start_row,
                block.span.output_start_row,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        decoded,
        carried_blanked_blocks(desk, local_id),
        "{}: session {local_id}'s blocks a full clear blanked decode as the clear flags its \
         producer wrote",
        desk.label()
    );
}

/// [`assert_history_imports`]' check of the meta's state in the hydrated, imported `engine`: every
/// application colour is live, the completed-command count is the parent's, the carried blocks
/// keep the cleared flags the parent recorded, and each carried mark names its prompt row (every
/// shell on these desks prompts as `dev@devhost`) and a readable output end, which holds only if
/// the successor continued the parent's absolute numbering through the import. A repainted
/// session is blank until its program redraws, so its marks are not read; nor is a prompt row a
/// full clear blanked that no later prompt drew on.
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
    // A full clear keeps the numbers of the rows it blanks on every mark and block and flags the
    // blocks it blanked (`text_cleared`, `command_text_cleared`; aterm-core's `note_full_clear`),
    // whose text then reads Evicted, as in the live terminal. The carried blocks keep the flags
    // the parent recorded, no more and no fewer: a consumer that drops one shows text a clear
    // took, and one that adds one hides text that is there.
    let recorded: Vec<&aterm_core::terminal::OutputBlockRepr> = checkpoint
        .shell
        .output_blocks
        .iter()
        .chain(&checkpoint.shell.current_block)
        .collect();
    assert_eq!(
        engine
            .all_blocks()
            .map(|block| (block.id, block.text_cleared, block.command_text_cleared))
            .collect::<Vec<_>>(),
        recorded
            .iter()
            .map(|block| (block.id, block.text_cleared, block.command_text_cleared))
            .collect::<Vec<_>>(),
        "{label}: session {id}'s carried blocks keep the cleared flags its parent recorded"
    );
    if repaint {
        return;
    }
    // From v0.106.0 a carry keeps a mark while any row it names is carried
    // (804646426f), so a prompt row older than the successor's oldest row (a
    // prompt above a visible-only carry's screen) reads Evicted, as in the
    // live terminal.
    let oldest = engine.main_grid().oldest_absolute_row();
    // A blanked mark's prompt row holds whatever the screen drew there since the clear, so it is
    // not read, unless a block no clear blanked starts its prompt on that row: v0.108.0's
    // `engine-state` shell that ran `clear` after six commands has the `clear` block on the top
    // row, where the next prompt drew. Which marks are blanked is the parent's record, never this
    // engine's, so a consumer that flags a block cannot switch a check off.
    let blanked = |block: &aterm_core::terminal::OutputBlockRepr| {
        block.text_cleared || block.command_text_cleared
    };
    let prompted: Vec<u64> = recorded
        .iter()
        .filter(|block| !blanked(block))
        .map(|block| block.span.prompt_start_row)
        .collect();
    for (mark, carried) in engine
        .command_marks()
        .iter()
        .zip(&checkpoint.shell.command_marks)
    {
        assert_eq!(
            (
                mark.prompt_start_row,
                mark.prompt_start_col,
                mark.command_start_row,
                mark.command_start_col,
                mark.output_start_row,
                mark.output_end_row,
                mark.exit_code
            ),
            (
                carried.prompt_start_row,
                carried.prompt_start_col,
                carried.command_start_row,
                carried.command_start_col,
                carried.output_start_row,
                carried.end_row,
                carried.exit_code
            ),
            "{label}: session {id}'s carried mark keeps its exact rows, columns and exit"
        );
        let cleared = !prompted.contains(&mark.prompt_start_row)
            && recorded.iter().any(|block| {
                blanked(block)
                    && block.span.prompt_start_row == mark.prompt_start_row
                    && block.span.output_start_row == mark.output_start_row
                    && block.span.end_row == mark.output_end_row
            });
        if mark.prompt_start_row >= oldest {
            let prompt = text_at(engine, mark.prompt_start_row);
            assert!(
                prompt.is_some(),
                "{label}: session {id}'s prompt row is readable"
            );
            if !cleared {
                assert!(
                    prompt
                        .as_deref()
                        .is_some_and(|text| text.contains("dev@devhost")),
                    "{label}: session {id}'s uncleared mark at row {} names its prompt line: {prompt:?}",
                    mark.prompt_start_row
                );
            }
        }
        if let Some(end) = mark.output_end_row {
            assert!(
                text_at(engine, end).is_some(),
                "{label}: session {id}'s mark ends its output on a readable row {end}"
            );
        }
    }
}

/// The meta's state is reassembled (v0.95.0 on): this build reads, from the older producer's meta,
/// the absolute row counters, whether an application colour diff rides it, and the
/// shell-integration phase, marks and count the parent recorded. Without this a consumer that
/// dropped a meta key would pass every other check here: the grids are sidecars, and the digests
/// are over bytes.
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
    if let Some(state) = expected.output_state {
        assert_eq!(
            !checkpoint.output_state.is_default(),
            state,
            "{label}: session {id}'s in-flight output state crosses"
        );
    }
}

/// The render debt crosses (v0.101.0 on): the successor owes the program exactly what its
/// parent's engine owed it, so the same epoch judge and repaint nudge judge the tab. That is the
/// rows `parent.toml` recorded (`render_debt`) and the columns the session's record carries
/// (`cut`, [`carried_render_debt_cuts`]); a session that carries neither owes nothing.
fn assert_render_debt_crosses(
    label: &str,
    session: &SessionExpectation,
    cut: u64,
    adopted: Option<aterm_core::terminal::RenderDebt>,
) {
    if let Some(recorded) = session.render_debt_cut {
        assert_eq!(
            recorded, cut,
            "{label}: the recorded column debt is the wire's"
        );
    }
    let cut_columns = u16::try_from(cut).expect("the producer's column debt fits its engine");
    assert!(
        cut_columns <= 4096,
        "{label}: column debt is within a screen"
    );
    let expected = match session.render_debt.as_deref() {
        Some(row) => {
            let mut debt = recorded_render_debt(label, row);
            debt.cut = cut_columns;
            Some(debt)
        }
        None if cut != 0 => Some(aterm_core::terminal::RenderDebt {
            shift: 0,
            pushed: 0,
            cut: cut_columns,
        }),
        None => None,
    };
    assert_eq!(
        adopted, expected,
        "{label}: session {} owes its program the rows and columns its parent owed it",
        session.local_id
    );
}

/// The rows of render debt `parent.toml` recorded for a session, `"<shift>,<pushed>"`, read here
/// rather than by the consumer's parser, which is what is checked.
fn recorded_render_debt(label: &str, row: &str) -> aterm_core::terminal::RenderDebt {
    row.split_once(',')
        .and_then(|(shift, pushed)| {
            Some(aterm_core::terminal::RenderDebt::of_rows(
                shift.parse().ok()?,
                pushed.parse().ok()?,
            ))
        })
        .unwrap_or_else(|| panic!("{label}: a recorded render debt reads: {row:?}"))
}

/// The columns of render debt each session's record carries, by local id: the record's own
/// `render_debt_cut` key, read off the manifest's bytes as a TOML tree, never by the consumer's
/// record. Older parent summaries omitted this additive key, so the record is where the
/// parent committed to it: v0.108.0's `engine-state` htop, the first desk
/// record to carry the key, owes 19 columns and no row. A record without the key owes no columns:
/// v0.107.0's producer is the first to write it, and only on a session that owes columns.
fn carried_render_debt_cuts(desk: &Desk) -> std::collections::BTreeMap<u64, u64> {
    let wire = String::from_utf8(desk.read(&desk.parent.manifest)).expect("UTF-8 manifest");
    wire_render_debt_cuts(&desk.label(), &wire)
}

/// Independently read additive debt from immutable producer bytes, requiring unique,
/// nonnegative session identities and counts.
fn wire_render_debt_cuts(label: &str, manifest: &str) -> std::collections::BTreeMap<u64, u64> {
    let (_, wire) = manifest.split_once('\n').expect("nonce header");
    let tree = aterm_toml::from_str::<aterm_toml::Value>(wire)
        .unwrap_or_else(|error| panic!("{label}: manifest TOML: {error}"));
    let sessions = tree
        .as_table()
        .and_then(|table| table.get("sessions"))
        .and_then(aterm_toml::Value::as_array)
        .unwrap_or_else(|| panic!("{label}: the wire has sessions"));
    let mut cuts = std::collections::BTreeMap::new();
    for session in sessions {
        let count = |key| {
            session
                .get(key)
                .and_then(aterm_toml::Value::as_integer)
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or_else(|| panic!("{label}: {key} is a nonnegative count"))
        };
        let id = count("local_id");
        let cut = if session.get("render_debt_cut").is_some() {
            count("render_debt_cut")
        } else {
            0
        };
        assert!(
            cuts.insert(id, cut).is_none(),
            "{label}: unique session {id}"
        );
    }
    cuts
}

/// One carried Settings draft as `(leaf, key, text)`. The leaf is the TOML path of the table that
/// holds the draft's `settings_drafts` list: `windows[w].restored_tabs[t].root`, then `.first` or
/// `.second` for each split, then `.view`.
type LeafDraft = (String, String, String);

/// Every Settings draft the parent's layout sidecar carries, with its leaf, read off the bytes,
/// never by the consumer (whose parse, sanitize and per-view bound are what the guard checks).
/// Two readings must agree: the `[[….settings_drafts]]` table headers counted in the raw text (how
/// v0.98.0's producer spells them) and every `settings_drafts` list in the TOML value tree. A draft
/// the count cannot see makes them disagree, so the guard stops rather than ask this build for
/// fewer drafts than the parent sent.
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

/// Every Settings draft on this build's placed layout, with the leaf that holds it, spelled as
/// [`wire_settings_drafts`] spells the parent's. [`assert_settings_drafts_placed`] checks the walk
/// against [`crate::restore::RestoreManifest::carried_settings_drafts`], so a draft kept where the
/// walk does not look is a red guard.
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

/// The Settings drafts are placed (v0.98.0 on): every unsaved draft the parent's layout carries is
/// on this build's placed layout, on the leaf that carried it, key and text intact, and none is
/// dropped as unreadable, or Settings would reopen without the person's text on every hop from
/// that release. Returns how many drafts the parent carried. Whether Settings still has an input
/// for each key when the restore reopens it (`App::reopen_carried_settings_drafts`) is past what a
/// fixture sees.
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
    // `take_incoming` fills `unplaced_settings_drafts` only when it cannot place the layout; this
    // keeps a consumer that starts to fill it beside a placed layout red.
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
/// ([`assert_history_imports`]), a held pane's too
/// ([`assert_held_history_imports`]).
///
/// FOUR CHILD PROCESSES, EVERY FOURTH DESK EACH ([`run_in_children`]), side by side: each shard
/// prints how many Settings drafts it placed, and this test holds their sum to the same floor.
#[test]
fn every_shipped_producers_desk_adopts_exactly_and_proves() {
    const SHARDS: usize = 4;
    const SHARD: &str = "ATERM_SHIPPED_DESK_SHARD";
    const DRAFTS: &str = "settings drafts placed: ";
    let Some(shard) = std::env::var(SHARD)
        .ok()
        .map(|text| text.parse::<usize>().expect("the shard is a number"))
    else {
        let drafts_carried = run_in_children(SHARD, SHARDS)
            .iter()
            .map(|stdout| {
                stdout
                    .lines()
                    .find_map(|line| line.split_once(DRAFTS)?.1.trim().parse::<usize>().ok())
                    .expect("every shard prints the drafts it placed")
            })
            .sum::<usize>();
        assert!(
            drafts_carried > 0,
            "the guard reads at least one shipped Settings draft (v0.98.0/link-dense-history)"
        );
        return;
    };
    let Some(_env) = env_alone() else { return };
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
    for desk in desks.iter().skip(shard).step_by(SHARDS) {
        drafts_carried += assert_desk_adopts_exactly_and_proves(desk);
    }
    println!("{DRAFTS}{drafts_carried}");
}

/// THE GUARD OVER THIS BUILD'S OWN DESKS: every desk the generator just wrote in `dir`
/// (`fixture_generator::generate_handoff_fixtures`), filed as this build's release, through
/// [`assert_desk_adopts_exactly_and_proves`]. The generator's self-check adopts each desk with
/// its own reader; this asks what the guard asks once the desks are checked in, so a desk the
/// guard cannot read as written (a key the guard takes from `parent.toml` that the generator
/// never copies there, a state the guard's checks do not expect) fails the generator's own run:
/// every ordinary test run of the tree that writes it, and the release cut that stages it,
/// before it lands on main. The caller holds [`ENV_LOCK`] and a [`restore_env`] guard, as the
/// generator does.
pub(super) fn assert_written_desks_adopt(dir: &std::path::Path) {
    let release = format!("v{}", crate::build_info::version());
    let desks = release_desks(&release, dir);
    assert!(
        !desks.is_empty(),
        "the generator wrote desks in {}",
        dir.display()
    );
    for desk in &desks {
        assert_desk_adopts_exactly_and_proves(desk);
    }
}

/// One desk through [`every_shipped_producers_desk_adopts_exactly_and_proves`]'s checks, the
/// Settings drafts it placed counted: the shipped desks, and the desks this build's generator
/// writes ([`assert_written_desks_adopt`]).
fn assert_desk_adopts_exactly_and_proves(desk: &Desk) -> usize {
    let label = desk.label();
    assert_eq!(
        format!("v{}", desk.parent.producer_version),
        desk.release,
        "{label}: filed under the release that wrote it"
    );
    assert_fixture_is_self_consistent(desk);
    assert_proof_function_is_the_parents(desk);

    let mut staged = stage(desk, None);
    let mut incoming = take_incoming_as(ReceiverShape::Current);
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
    let drafts = assert_settings_drafts_placed(desk, &incoming);
    assert_held_panes_shown(desk, &incoming);
    let held = std::mem::take(&mut incoming.held);
    let cuts = carried_render_debt_cuts(desk);
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
        assert_clear_flags_decoded(desk, expected.local_id, checkpoint);
        assert_render_debt_crosses(
            &label,
            expected,
            *cuts.get(&expected.local_id).expect("session on the wire"),
            adopted.render_debt,
        );
        // The shell's mark authority crosses: the requirement and the nonce the running shell
        // signs with, or every mark is dropped for the session's life, or accepted unsigned.
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
        crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0),
        crate::build_info::git_commit(),
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
    assert_held_history_imports(desk, held);
    drop(ready);
    staged.ready_write = None;
    staged.teardown();
    drafts
}

/// The meta check is not vacuous: it catches what nothing else in the guard can, a consumer that
/// stops reading the shell-integration state while the wire is the parent's own. The newest
/// `colour-and-shell` desk is staged unmodified, so both digests and the proof are the parent's;
/// the one change is session 0's adopted checkpoint with its shell state dropped, as a consumer
/// that no longer read the `shell` key would leave it. Every other check passes over it (geometry,
/// grid bytes, control carry, digests, proof), and [`assert_meta_state_crosses`] fails. (Deleting
/// `shell` from the staged meta instead would move the screen digest and prove nothing about the
/// meta check.)
#[test]
fn a_consumer_that_drops_the_shell_state_fails_only_the_meta_check() {
    let Some(_env) = env_alone() else { return };
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
        crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0),
        crate::build_info::git_commit(),
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

/// The render-debt check holds the columns too: no digest or proof covers a record's
/// `render_debt_cut` (the key sits beside the hashed meta and grids), so only that check can fail
/// a consumer that stops reading it. The newest desk whose record carries a cut is staged
/// unmodified and passes; its adoption with the cut dropped, as such a consumer would leave it,
/// fails [`assert_render_debt_crosses`].
#[test]
fn a_consumer_that_drops_column_debt_fails_the_exact_debt_check() {
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let desk = every_desk()
        .into_iter()
        .find(|desk| desk.release == "v0.108.0" && desk.name == "engine-state")
        .expect("the frozen column-debt desk");
    let label = desk.label();
    let expected = desk.expected(8);
    let wire = String::from_utf8(desk.read(&desk.parent.manifest)).expect("UTF-8 manifest");
    let cuts = wire_render_debt_cuts(&label, &wire);
    let cut = *cuts.get(&8).expect("session 8 on the wire");
    assert_eq!(cut, 19, "the immutable producer carries a cut-only debt");
    assert!(
        expected.render_debt.is_none(),
        "its parent summary has no row debt"
    );
    let mut staged = stage(&desk, None);
    let mut incoming = take_incoming_as(ReceiverShape::Current);
    let adopted = incoming
        .adopted
        .iter_mut()
        .find(|session| session.local_id == 8)
        .expect("session 8 adopts");
    assert_render_debt_crosses(&label, expected, cut, adopted.render_debt);
    adopted.render_debt = None;
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_render_debt_crosses(&label, expected, cut, adopted.render_debt);
    }));
    assert!(
        caught.is_err(),
        "dropping the carried columns must fail exact adoption"
    );
    let ((proof, ready, _), _sessions) =
        child_proof_from(incoming).expect("the child adopts and proves");
    let parent_expects = adoption_proof(
        &desk.parent.nonce,
        crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0),
        crate::build_info::git_commit(),
        &unhex::<32>(&label, &desk.parent.layout_digest),
        &unhex::<32>(&label, &desk.parent.screen_digest),
        &staged.live,
    )
    .expect("the parent's expectation");
    assert_eq!(
        proof, parent_expects,
        "the ordinary proof cannot see the dropped debt"
    );
    drop(ready);
    staged.ready_write = None;
    staged.teardown();
}

/// The mark check reads a full clear off the parent's record, not off the engine it judges, and it
/// is not vacuous. The newest `engine-state` desk's shell that ran `clear` (v0.108.0 on) and one
/// of its shells no clear reached land as carried and pass. Each consumer below changes only what
/// its restore makes of one session's checkpoint, the check is handed that checkpoint as the
/// parent recorded it, and each fails [`assert_meta_state_lands`]: one that drops the cleared
/// flags (the blanked text would read again), one that flags every block of a session whose
/// parent recorded none (its text would read Evicted), and one that moves the cleared session's
/// marks and blocks down a row (their flags travel with them, so a check that took which marks a
/// clear blanked from the engine would read none of their prompt rows and pass it).
#[test]
fn a_consumer_that_moves_a_full_clears_flags_or_rows_fails_the_mark_check() {
    type Checkpoint = aterm_core::terminal::TerminalCheckpoint;
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let desk = every_desk()
        .into_iter()
        .find(|desk| desk.release == "v0.108.0" && desk.name == "engine-state")
        .expect("the frozen full-clear desk");
    let label = desk.label();
    let mut staged = stage(&desk, None);
    let incoming = take_incoming_as(ReceiverShape::Current);
    let flags = |checkpoint: &Checkpoint| {
        checkpoint
            .shell
            .output_blocks
            .iter()
            .chain(&checkpoint.shell.current_block)
            .map(|block| block.text_cleared || block.command_text_cleared)
            .collect::<Vec<_>>()
    };
    // The first exactly adopted shell with marks and blocks that a clear did (or did not) blank.
    let carried = |cleared: bool| {
        incoming
            .adopted
            .iter()
            .find_map(|adopted| {
                let checkpoint = adopted.checkpoint.as_ref().filter(|_| !adopted.repaint)?;
                let flags = flags(checkpoint);
                (!checkpoint.shell.command_marks.is_empty()
                    && !flags.is_empty()
                    && flags.contains(&true) == cleared)
                    .then(|| (adopted.local_id, checkpoint.clone()))
            })
            .unwrap_or_else(|| {
                panic!(
                    "PRECONDITION: {label} carries a shell with marks a clear blanked: {cleared}"
                )
            })
    };
    let (cleared_id, cleared) = carried(true);
    let (uncleared_id, uncleared) = carried(false);
    for (id, recorded) in [(cleared_id, &cleared), (uncleared_id, &uncleared)] {
        assert_meta_state_lands(
            &label,
            id,
            recorded,
            false,
            &Terminal::from_checkpoint(recorded),
        );
    }
    // Whether the check passes session `id`, carried as `recorded`, by a consumer whose restore
    // leaves its engine as `restore` leaves the checkpoint.
    let passes = |id: u64, recorded: &Checkpoint, restore: &dyn Fn(&mut Checkpoint)| {
        let mut restored = recorded.clone();
        restore(&mut restored);
        let engine = Terminal::from_checkpoint(&restored);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_meta_state_lands(&label, id, recorded, false, &engine);
        }))
        .is_ok()
    };
    let unflag = |checkpoint: &mut Checkpoint| {
        let shell = &mut checkpoint.shell;
        for block in shell
            .output_blocks
            .iter_mut()
            .chain(&mut shell.current_block)
        {
            block.text_cleared = false;
            block.command_text_cleared = false;
        }
    };
    assert!(
        !passes(cleared_id, &cleared, &unflag),
        "the guard must fail a consumer that drops the cleared flags its parent recorded"
    );
    let flag = |checkpoint: &mut Checkpoint| {
        let shell = &mut checkpoint.shell;
        for block in shell
            .output_blocks
            .iter_mut()
            .chain(&mut shell.current_block)
        {
            block.text_cleared = true;
        }
    };
    assert!(
        !passes(uncleared_id, &uncleared, &flag),
        "the guard must fail a consumer that flags blocks no clear blanked"
    );
    let move_down = |checkpoint: &mut Checkpoint| {
        let shell = &mut checkpoint.shell;
        let spans = shell
            .command_marks
            .iter_mut()
            .chain(&mut shell.current_mark)
            .chain(
                shell
                    .output_blocks
                    .iter_mut()
                    .chain(&mut shell.current_block)
                    .map(|block| &mut block.span),
            );
        for span in spans {
            span.prompt_start_row += 1;
            for row in [
                &mut span.command_start_row,
                &mut span.output_start_row,
                &mut span.end_row,
            ]
            .into_iter()
            .flatten()
            {
                *row += 1;
            }
        }
    };
    assert!(
        !passes(cleared_id, &cleared, &move_down),
        "the guard must fail a consumer that moves a cleared session's marks off their rows"
    );
    let ((proof, ready, _), _sessions) =
        child_proof_from(incoming).expect("the child adopts and proves");
    let parent_expects = adoption_proof(
        &desk.parent.nonce,
        crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0),
        crate::build_info::git_commit(),
        &unhex::<32>(&label, &desk.parent.layout_digest),
        &unhex::<32>(&label, &desk.parent.screen_digest),
        &staged.live,
    )
    .expect("the parent's expectation");
    assert_eq!(
        proof, parent_expects,
        "the parent's bytes and proof stay exact"
    );
    drop(ready);
    staged.ready_write = None;
    staged.teardown();
}

/// The flag check reads the producer's bytes, not this build's decode of them, and a reader that
/// lost the clear's flags fails it. The newest `engine-state` desk's shell that ran `clear` is
/// staged unmodified, and its adopted checkpoint passes what the guard runs on one session's meta
/// ([`assert_clear_flags_decoded`], then [`assert_meta_state_lands`] on the engine restored from
/// it). The same checkpoint as a reader that dropped the flags would decode it, with the engine
/// restored from that decode, fails where the decoded flags are held to the producer's meta.
/// [`assert_meta_state_lands`] alone takes the flags from the decode, so it would fail that reader
/// only on the blank prompt row the clear left, blaming the marks' numbering.
#[test]
fn a_consumer_that_decodes_a_meta_without_its_clear_flags_fails_the_flag_check() {
    type Checkpoint = aterm_core::terminal::TerminalCheckpoint;
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let desk = every_desk()
        .into_iter()
        .rfind(|desk| desk.name == "engine-state")
        .expect("an engine-state desk");
    let label = desk.label();
    let mut staged = stage(&desk, None);
    let incoming = take_incoming_as(ReceiverShape::Current);
    let (id, decoded) = incoming
        .adopted
        .iter()
        .find_map(|adopted| {
            let checkpoint = adopted.checkpoint.as_ref().filter(|_| !adopted.repaint)?;
            (!carried_blanked_blocks(&desk, adopted.local_id).is_empty())
                .then(|| (adopted.local_id, checkpoint.clone()))
        })
        .unwrap_or_else(|| panic!("PRECONDITION: {label} carries a shell a clear blanked"));
    // What the guard runs on one session's meta for a consumer whose reader decodes it as
    // `decoded`: `None` when it passes, else what it said.
    let guard = |decoded: &Checkpoint| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_clear_flags_decoded(&desk, id, decoded);
            assert_meta_state_lands(
                &label,
                id,
                decoded,
                false,
                &Terminal::from_checkpoint(decoded),
            );
        }))
        .err()
        .map(|payload| {
            payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(ToString::to_string))
                .unwrap_or_default()
        })
    };
    assert_eq!(
        guard(&decoded),
        None,
        "{label}: session {id} as this build decodes it"
    );
    // The reader that lost the clear's flags.
    let mut lost = decoded;
    let shell = &mut lost.shell;
    for block in shell
        .output_blocks
        .iter_mut()
        .chain(&mut shell.current_block)
    {
        block.text_cleared = false;
        block.command_text_cleared = false;
    }
    let said = guard(&lost);
    assert!(
        said.as_deref()
            .is_some_and(|said| said.contains("decode as the clear flags its producer wrote")),
        "the guard must fail a reader that lost the clear's flags where it holds them to the \
         producer's meta: {said:?}"
    );
    let ((_, ready, _), _sessions) =
        child_proof_from(incoming).expect("the child adopts and proves");
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

/// The drafts check is not vacuous: [`assert_settings_drafts_placed`] fails a consumer that loses
/// a carried Settings draft, or places it on another leaf, while the wire is the parent's own. The
/// newest `link-dense-history` desk (a Settings tab with one unsaved draft) is staged unmodified,
/// so both digests are the parent's; each change is to this build's placed layout, put back
/// before the next:
///
/// - the draft dropped without a count, as a sanitize that cleared the list would leave it;
/// - the draft dropped and counted unreadable, as a tightened `settings_draft_is_carriable` or
///   bound would leave it;
/// - the Settings tab placed in the other tab's position: every draft's key and text are still on
///   the layout, so only the leaf differs.
#[test]
fn a_consumer_that_loses_or_moves_a_settings_draft_fails_the_drafts_check() {
    let Some(_env) = env_alone() else { return };
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
        crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0),
        crate::build_info::git_commit(),
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

/// Every release pins each required desk from that desk's floor on, and each is checked in. The
/// cutter refuses a cut whose predecessor misses one (`aterm-release`'s
/// `gates::handoff_fixtures_of`); this is the same rule over the tree as it stands, red before
/// anyone tries to cut.
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

/// THE SUCCESSOR SAYS WHAT DEGRADED (the 2026-09-22/23 update audit, plan
/// P1-5). The v0.91.0 incident desk with its shell's meta rewritten to hold a
/// NUL in the directory — what an older producer carried for an OSC 7 `%00` —
/// still adopts both sessions, and the count the landing record reads
/// (`IncomingHandoff::repainted_tabs`) is the one session this build
/// repainted.
/// Before, the degrade was a log line only: the user found a blank tab with
/// no word about why.
#[test]
fn a_repainted_session_is_counted_for_the_landing_row() {
    let Some(_env) = env_alone() else { return };
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
    staged.teardown();
}

// ------------------------------------------------- the rendezvous transport
//
// macOS-only, as `crate::handoff_rendezvous` is: the launched lane and the PTY-device proof term
// exist only there.

/// Every release whose rendezvous transport is pinned (`v<release>/rendezvous.toml`, written by
/// that release's own claim, grant and proof code through the generator): every release
/// [`PINNED_DESKS`] pins, in its order, so one table pins both and a lost file is a red test.
#[cfg(target_os = "macos")]
fn pinned_rendezvous() -> Vec<&'static str> {
    let mut releases = PINNED_DESKS
        .iter()
        .map(|(release, _)| *release)
        .collect::<Vec<_>>();
    releases.dedup();
    releases
}

/// The fixed inputs every rendezvous capture runs over, in every release and in this build, so a
/// record holds only the producer's spelling of them. The generator records over these constants.
#[cfg(target_os = "macos")]
pub(super) const RZ_SECRET: &str =
    "3f9c2e71a0b84d56e1f7c3a92b0d4e68f5a1c7e39b2d0f86a4e1c5b7d93f0a2c";
#[cfg(target_os = "macos")]
pub(super) const RZ_NONCE: &str = "c0ffee00aa55aa55c0ffee00aa55aa55";
#[cfg(target_os = "macos")]
pub(super) const RZ_SESSIONS: [(u64, i32); 3] = [(0, 4000), (1, 4001), (7, 4002)];

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

/// What one build's rendezvous code does with the fixed inputs.
#[cfg(target_os = "macos")]
pub(super) struct TransportCapture {
    pub(super) claim_frame: Vec<u8>,
    pub(super) grant_header: Vec<u8>,
    pub(super) grant_body: String,
    pub(super) descriptor_order: Vec<String>,
    pub(super) rendezvous_file_shape: String,
    pub(super) claim_hex_len: usize,
}

/// Lowercase hex of `bytes`.
#[cfg(target_os = "macos")]
pub(super) fn hex_of(bytes: &[u8]) -> String {
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

/// A fresh PTY: `(master, slave)`, both close-on-exec from birth.
#[cfg(target_os = "macos")]
pub(super) fn open_pty_pair() -> (i32, i32) {
    aterm_pty::open_pty_pair().expect("open_pty_pair")
}

/// This build's transport over the fixed inputs, observed from outside: the claim its dialer
/// writes (read off a plain listener), and the grant its parent sends over a real
/// [`crate::handoff_rendezvous::Rendezvous`] to a real claim (the header and every descriptor
/// dequeued by one `recv_with_fds`, the body, and each descriptor identified by what it is: a
/// master by its `st_rdev`, a pipe end by its inode). The generator records each release with this
/// same function.
#[cfg(target_os = "macos")]
pub(super) fn capture_transport(scratch: &std::path::Path) -> TransportCapture {
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
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let own = i32::try_from(std::process::id()).expect("a pid fits pid_t");
    let peer = crate::handoff_rendezvous::admitted(
        rendezvous.accept_claim(
            Some(own),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            deadline,
            &|| false,
        ),
        "the claim is accepted",
    );
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
        None,
    )
    .expect("the grant is sent");
    dialer
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
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

/// The stub parent: accept one dial on `listener`, check that its claim is `expected_claim`, and
/// send `header` carrying `fds`, then `body`. The stream is handed back to be held until the
/// successor has answered, as a real parent holds it through the proof wait (Darwin refuses a
/// socket option on a stream whose peer has gone, and the successor sets one per read).
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

/// This build's successor, dialled at a stub parent that replays one release's recorded grant
/// (its header and body byte for byte, fresh PTY masters and pipe ends in that release's
/// descriptor order). The stub checks the claim this build presents against the one that
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

/// The rendezvous transport is the shipped one. The desk guard pins the manifests and sidecars,
/// but on macOS the successor gets every descriptor, and the parent its claim, over the
/// rendezvous, so a change to a magic, the header, the body or the descriptor order would pass
/// every same-build test and fail every update in the field. For every release:
///
/// - this build's claim for the recorded secret is the one that release's parent expects, and
///   this build's grant for the recorded nonce and sessions is the one that release's successor
///   reads (header, body and descriptor order), so a grant of up to 62 sessions keeps its shape;
/// - this build's successor, dialled at a stub parent replaying that release's grant, pairs every
///   session with the master sent for it and takes the readiness and Commit ends it was sent;
/// - the launch environment's names are that release's, and the values its parent published (its
///   target, its birth record, the proof-term spellings) parse here;
/// - the adoption proof over that release's recorded PTY device terms is its own recorded proof.
#[cfg(target_os = "macos")]
#[test]
fn every_shipped_producers_rendezvous_grants_this_successor() {
    let records = every_rendezvous_record();
    let releases = records
        .iter()
        .map(|(release, _)| release.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        releases,
        pinned_rendezvous(),
        "every pinned release's rendezvous.toml is checked in, and every checked-in one is pinned"
    );
    let scratch = crate::test_scratch::short_dir("the claim's socket");
    let this = capture_transport(scratch.path());
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
                crate::handoff_rendezvous::ENV_RENDEZVOUS.name(),
                crate::handoff_rendezvous::ENV_CLAIM.name(),
                crate::handoff_rendezvous::ENV_PROOF_TERM.name(),
                ENV_TARGET.name(),
                ENV_PARENT_BIRTH.name(),
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
            Ok(AdoptionProof {
                count: record.proof_count,
                digest: unhex::<32>(&label, &record.proof_digest),
            }),
            "{label}: this build's proof over that release's device terms is its own"
        );
        replay_grant_to_this_successor(&label, record, scratch.path());
    }
}

/// A v0.97.0 policy desk is said as the policy's request. v0.97.0 follows a successor's
/// no-scrollback policy (no line in any screen, no sidecar, every line counted as left behind) but
/// predates `history_withheld`, the per-record word that the loss was asked for. This build knows
/// it from its own sealed policy: when that policy asked v0.97.0's build for no scrollback, the
/// band's row is the request's, never the failure row's "everything on screen and the newest lines
/// came across", which is false under `repaint` and `visible`.
///
/// Negative controls: with no policy of its own, with one that leaves v0.97.0's build out of its
/// range, or with one that carries scrollback, the same desk is said as a loss.
///
/// And only v0.97.0: under a policy whose range spans v0.93.0 to v0.99, the same records as a
/// v0.95.0 producer's (which follows no policy) and the v0.97.0 desk with one record naming a
/// sidecar are each the failure row, as is a v0.98.0 policy desk without its `history_withheld`
/// word; the untouched v0.97.0 desk is still the request, and the untouched v0.98.0 one says it
/// by its own word.
#[test]
fn a_v097_policy_desk_is_said_as_the_policys_request() {
    use aterm_update_core::handoff_policy::{CarryCeiling, HandoffPolicy, ProducerRange};
    let Some(_env) = env_alone() else { return };
    let _restore = restore_env();
    let desks = every_desk();
    // The band row for `desk` adopted under `own` as this build's policy, its
    // manifest rewritten by `rewrite` first.
    let row_for_rewritten =
        |desk: &Desk, own: Option<HandoffPolicy>, rewrite: Option<&dyn Fn(&mut SessionHandoff)>| {
            let _own = own_policy_for_test(own);
            let staged = stage_with(desk, None, rewrite);
            let incoming = take_incoming_as(ReceiverShape::Current);
            assert_eq!(
                incoming.adopted.len(),
                desk.parent.session.len(),
                "{}: every session adopts",
                desk.label()
            );
            let loss = crate::handoff_history::carried_loss(
                incoming.adopted.iter().map(|adopted| &adopted.history),
            );
            drop(incoming);
            staged.teardown();
            (
                loss,
                crate::update_words::scrollback_left_behind(
                    loss.lines,
                    loss.tabs,
                    loss.withheld,
                    loss.held,
                    loss.held_failed,
                ),
            )
        };
    let row_for = |desk: &Desk, own: Option<HandoffPolicy>| row_for_rewritten(desk, own, None);
    // A range from v0.93.0 to just before v0.100.0.
    let spanning = ProducerRange {
        min_build: 1_790_305_290,
        max_build: 1_790_807_621,
    };
    for (name, carry) in [
        ("policy-repaint", CarryCeiling::Repaint),
        ("policy-visible", CarryCeiling::Visible),
    ] {
        let desk = desks
            .iter()
            .find(|desk| desk.release == "v0.97.0" && desk.name == name)
            .unwrap_or_else(|| panic!("the v0.97.0 {name} desk is checked in"));
        let label = desk.label();
        let producer = SessionHandoff::from_toml(
            String::from_utf8(desk.read(&desk.parent.manifest))
                .expect("UTF-8 manifest")
                .split_once('\n')
                .expect("nonce header")
                .1,
        )
        .and_then(|manifest| manifest.outgoing_build)
        .expect("v0.97.0 names its build");
        let ours = |range: ProducerRange| HandoffPolicy {
            applies_to_producers: Some(range),
            carry: Some(carry),
            park_quiet_gate_at_land: None,
        };
        let covering = ProducerRange {
            min_build: producer,
            max_build: producer,
        };
        let (loss, row) = row_for(desk, Some(ours(covering)));
        assert!(
            loss.lines > 0 && loss.tabs > 0,
            "PRECONDITION: {label} leaves lines behind ({loss:?})"
        );
        assert_eq!(
            loss.withheld, loss.lines,
            "{label}: every line this build's policy asked v0.97.0 to leave is its request"
        );
        assert_eq!(
            row,
            crate::update_words::scrollback_withheld(loss.lines, loss.tabs),
            "{label}: the row says the new version asked for it"
        );
        for (control, own) in [
            ("no policy of its own", None),
            (
                "a policy for other builds",
                Some(ours(ProducerRange {
                    min_build: producer + 1,
                    max_build: producer + 1,
                })),
            ),
            (
                "a policy that carries scrollback",
                Some(HandoffPolicy {
                    carry: Some(CarryCeiling::Full),
                    ..ours(covering)
                }),
            ),
        ] {
            let (loss, row) = row_for(desk, own);
            assert_eq!(
                (loss.withheld, row),
                (
                    0,
                    crate::update_words::scrollback_lost(loss.lines, loss.tabs)
                ),
                "{label} under {control}: a loss this build did not ask for is said as before"
            );
        }

        // Only the producer that follows a policy without saying so.
        assert_eq!(
            producer,
            aterm_update_core::handoff_policy::FIRST_POLICY_FOLLOWING_BUILD,
            "{label}: the checked-in v0.97.0 desk is v0.97.0's build"
        );
        let spanning_policy = ours(spanning);
        let (loss, row) = row_for(desk, Some(spanning_policy));
        assert_eq!(
            (loss.withheld, row),
            (
                loss.lines,
                crate::update_words::scrollback_withheld(loss.lines, loss.tabs)
            ),
            "{label} under the spanning range: still the request"
        );
        let as_v095 = |manifest: &mut SessionHandoff| manifest.outgoing_build = Some(1_790_563_767);
        let named_a_sidecar = |manifest: &mut SessionHandoff| {
            let record = manifest
                .sessions
                .iter_mut()
                .find(|record| record.history_dropped > 0)
                .expect("a record that left lines behind");
            record.history = Some(format!("1 {} 1", "0".repeat(64)));
        };
        for (control, rewrite) in [
            (
                "a v0.95.0 producer, which follows no policy",
                &as_v095 as &dyn Fn(&mut SessionHandoff),
            ),
            (
                "a desk that named a scrollback sidecar",
                &named_a_sidecar as &dyn Fn(&mut SessionHandoff),
            ),
        ] {
            let (loss, row) = row_for_rewritten(desk, Some(spanning_policy), Some(rewrite));
            assert!(
                loss.lines > 0,
                "PRECONDITION: {label} as {control} loses lines"
            );
            assert_eq!(
                (loss.withheld, row),
                (
                    0,
                    crate::update_words::scrollback_lost(loss.lines, loss.tabs)
                ),
                "{label} as {control}: its unexplained loss is a failure, said as one"
            );
        }

        let v098 = desks
            .iter()
            .find(|desk| desk.release == "v0.98.0" && desk.name == name)
            .unwrap_or_else(|| panic!("the v0.98.0 {name} desk is checked in"));
        let (loss, row) = row_for(v098, Some(spanning_policy));
        assert_eq!(
            (loss.withheld, row),
            (
                loss.lines,
                crate::update_words::scrollback_withheld(loss.lines, loss.tabs)
            ),
            "{}: v0.98.0 says its withholding by its own word",
            v098.label()
        );
        let unsaid = |manifest: &mut SessionHandoff| {
            assert_eq!(
                manifest.outgoing_build,
                Some(crate::seamless::FIRST_WITHHELD_SAYING_BUILD),
                "the checked-in v0.98.0 desk is v0.98.0's build"
            );
            for record in &mut manifest.sessions {
                record.history_withheld = false;
            }
        };
        let (loss, row) = row_for_rewritten(v098, Some(spanning_policy), Some(&unsaid));
        assert!(loss.lines > 0, "PRECONDITION: {} loses lines", v098.label());
        assert_eq!(
            (loss.withheld, row),
            (
                0,
                crate::update_words::scrollback_lost(loss.lines, loss.tabs)
            ),
            "{} without its word: a v0.98.0 record that does not say it was withheld was not",
            v098.label()
        );
    }
}
