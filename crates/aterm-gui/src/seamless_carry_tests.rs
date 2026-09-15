// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CONTROL CARRY across a real handoff: `write_outgoing` with sidecars,
//! the successor's `take_incoming`, the adoption proof, and what drivers read
//! afterwards — `history`, `offscreen`, and `aterm drive report` itself. Plus
//! the rollback lane: the same new sender handing off to a receiver built
//! BEFORE the carry, proof and all.

use std::sync::{Arc, Mutex, PoisonError};

use aterm_core::terminal::{AltArchiveImport, Terminal, TerminalCheckpoint};

use super::tests::{
    ENV_LOCK, RestoreVar, StageCarry, StagedHandoff, child_proof_from, pipe_pair,
    stage_carry_handoff,
};
use super::*;
use crate::handoff_carry::{self, ControlCarry};
use crate::session_store::SessionRecord;
use crate::turn_ledger::{ArchMark, TurnLedger, TurnRecord};

// ------------------------------------------------ the receiver before the carry

/// `SessionRecord` exactly as the last build before the control carry declared
/// it (f1c76b519): no `control`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PreCarryRecord {
    local_id: u64,
    sid: String,
    parent: Option<String>,
    state: String,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    screen: Option<ScreenCarry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attention: Option<String>,
}

/// `SessionHandoff` exactly as that build declared it: no `next_turn_id`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PreCarryHandoff {
    schema: u32,
    sessions: Vec<PreCarryRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window: Option<WindowCarry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    connections: Vec<ConnectionCarry>,
}

impl PreCarryHandoff {
    /// That build's `SessionHandoff::from_toml`.
    fn from_toml(s: &str) -> Option<Self> {
        let h: Self = aterm_toml::from_str(s).ok()?;
        (h.schema == SessionHandoff::SCHEMA).then_some(h)
    }
}

/// How [`ReceiverShape::PreCarry`] parses: that build's types, lifted into
/// today's with nothing it could not have read.
pub(super) fn pre_carry_parse(toml: &str) -> Option<SessionHandoff> {
    let old = PreCarryHandoff::from_toml(toml)?;
    Some(SessionHandoff {
        schema: old.schema,
        window: old.window,
        connections: old.connections,
        next_turn_id: None,
        sessions: old
            .sessions
            .into_iter()
            .map(|r| SessionRecord {
                local_id: r.local_id,
                sid: r.sid,
                parent: r.parent,
                state: r.state,
                title: r.title,
                screen: r.screen,
                user_title: r.user_title,
                description: r.description,
                icon: r.icon,
                role: r.role,
                attention: r.attention,
                control: None,
            })
            .collect(),
    })
}

// ------------------------------------------------------------- the fake app

const ROWS: u16 = 30;
const COLS: u16 = 100;
const ORIGIN: u64 = 0x5eed_0001;

/// A fullscreen app shaped like Claude Code (synthetic, never a capture): a
/// transcript over a composer — a blank, a rule, `❯`, a rule, a footer.
struct FakeApp {
    doc: Vec<String>,
}

impl FakeApp {
    fn new() -> Self {
        Self {
            doc: (0..40)
                .map(|i| format!("⏺ an earlier answer, row {i:04}"))
                .collect(),
        }
    }

    fn screen(&self, rows: u16, cols: u16) -> Vec<String> {
        let t = usize::from(rows) - 5;
        let rule = "─".repeat(usize::from(cols) - 1);
        let mut out: Vec<String> = self.doc[self.doc.len().saturating_sub(t)..].to_vec();
        while out.len() < t {
            out.insert(0, String::new());
        }
        out.extend([
            String::new(),
            rule.clone(),
            "❯".to_string(),
            rule,
            "  ? for shortcuts".to_string(),
        ]);
        out
    }

    /// One DEC 2026 frame painting every row in place.
    fn frame(&self, rows: u16, cols: u16) -> Vec<u8> {
        let mut v = b"\x1b[?2026h\x1b[?25l".to_vec();
        for (r, text) in self.screen(rows, cols).iter().enumerate() {
            v.extend_from_slice(format!("\x1b[{};1H\x1b[2K{text}", r + 1).as_bytes());
        }
        v.extend_from_slice(b"\x1b[?25h\x1b[?2026l");
        v
    }

    /// Say `lines`, one frame per two rows, on `t`.
    fn say(&mut self, t: &mut Terminal, lines: impl IntoIterator<Item = String>) {
        let (rows, cols) = (t.rows(), t.cols());
        for (k, line) in lines.into_iter().enumerate() {
            self.doc.push(line);
            if k % 2 == 1 {
                t.process(&self.frame(rows, cols));
            }
        }
        t.process(&self.frame(rows, cols));
    }
}

fn numbered(range: std::ops::RangeInclusive<usize>) -> Vec<String> {
    range
        .map(|i| format!("⏺ counted {i:04} — the answer goes on"))
        .collect()
}

/// A live-shaped engine (what `new_live_terminal` does for the archive).
fn live_engine(origin: u64) -> Terminal {
    let mut t = Terminal::new(ROWS, COLS);
    t.set_alt_archive_enabled(true);
    t.set_alt_archive_origin(origin);
    t
}

const TURN_TEXT: &str = "count to 60 for me";

/// The old process: the app answered one turn with `n` numbered rows.
/// Returns its engine and ledger, as `SessionCtx` shares them.
fn old_session(app: &mut FakeApp, n: usize) -> (Arc<Mutex<Terminal>>, Arc<Mutex<TurnLedger>>) {
    let mut t = live_engine(ORIGIN);
    t.process(b"\x1b[?1049h");
    t.process(&app.frame(ROWS, COLS));
    let mut ledger = TurnLedger::default();
    ledger.push(TurnRecord {
        id: 40,
        started_ms: 1,
        dur_ms: 2,
        submitted: true,
        status: "settled",
        text: "an earlier turn".to_string(),
        screen_hash: 1,
        seq: 1,
        arch: ArchMark::of(&t),
        carried: false,
    });
    // The turn: its mark first, as `cmd_turn` takes it, then the echo of the
    // message and the answer.
    let arch = ArchMark::of(&t);
    app.say(&mut t, [format!("❯ {TURN_TEXT}")]);
    app.say(&mut t, numbered(1..=n));
    ledger.push(TurnRecord {
        id: 41,
        started_ms: 100,
        dur_ms: 2000,
        submitted: true,
        status: "settled",
        text: TURN_TEXT.to_string(),
        screen_hash: 2,
        seq: t.content_seq(),
        arch,
        carried: false,
    });
    (Arc::new(Mutex::new(t)), Arc::new(Mutex::new(ledger)))
}

/// The freeze and the worker, as the handoff runs them: the checkpoint and
/// the carry's head under one lock, then the export.
fn outgoing(
    term: &Arc<Mutex<Terminal>>,
    turns: &Arc<Mutex<TurnLedger>>,
    next_turn_id: Option<u64>,
) -> StageCarry {
    let (checkpoint, source) = {
        let guard = term.lock().unwrap();
        (
            guard.checkpoint_carry(256).expect("parser is Ground"),
            handoff_carry::capture_head(0, &guard, term, turns, true),
        )
    };
    StageCarry {
        screens: vec![checkpoint],
        controls: handoff_carry::export(&[source]),
        next_turn_id,
    }
}

/// `spawn_session`'s adoption, for one adopted session: the ledger, then the
/// engine restored and the archive installed.
fn adopt(
    checkpoint: &TerminalCheckpoint,
    control: Option<ControlCarry>,
) -> (Terminal, TurnLedger, Option<AltArchiveImport>) {
    let mut control = control;
    let ledger = control
        .as_mut()
        .map_or_else(TurnLedger::default, ControlCarry::ledger);
    let mut t = live_engine(0xfeed); // the new process's own origin
    t.restore_checkpoint(checkpoint);
    let outcome = control.and_then(|c| c.install(&mut t));
    (t, ledger, outcome)
}

fn env_guard() -> Vec<RestoreVar> {
    vec![
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

fn this_build() -> Option<String> {
    Some(encode_target_identity(
        crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0),
        crate::build_info::GIT_COMMIT,
    ))
}

/// Publish the staged handoff to the "successor" (this process), run
/// `consume` as it, and close the channels. `consume` gets the pipes' ends it
/// may need and returns what the test reads.
fn as_successor<T>(staged: &StagedHandoff, consume: impl FnOnce(i32) -> T) -> T {
    let (ready_read, ready_write) = pipe_pair("ready");
    let (commit_read, commit_write) = pipe_pair("commit");
    staged.publish_env(ready_write, commit_read, this_build());
    let out = consume(ready_read);
    for fd in [ready_read, commit_read, commit_write] {
        aterm_pty::close_fd(fd);
    }
    out
}

fn ctl_files(staged: &StagedHandoff) -> Vec<std::path::PathBuf> {
    let dir = staged.manifest_path.parent().unwrap().to_path_buf();
    let stem = staged
        .manifest_path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            n.starts_with(&stem) && n.ends_with(".ctl")
        })
        .collect()
}

// ------------------------------------------------------- `aterm drive report`

/// A `Ctl` over the real verbs: `history` from the session's ledger,
/// `offscreen` from its engine — the replies a raw client reads.
struct Host {
    ctx: Arc<crate::SessionCtx>,
    term: Arc<Mutex<Terminal>>,
    requests: Vec<String>,
}

impl aterm_agent::supervise::run::Ctl for Host {
    fn call(&mut self, args: &[&str]) -> Result<aterm_agent::supervise::run::CtlReply, String> {
        self.requests.push(args.join(" "));
        let rest = args[1..].join(" ");
        let reply = match args[0] {
            "history" => crate::control::cmd_history(&self.ctx, &rest),
            "offscreen" => crate::control::cmd_offscreen(&self.term, &rest),
            other => return Err(format!("unscripted verb {other}")),
        };
        Ok(if reply.starts_with("OK") {
            aterm_agent::supervise::run::CtlReply {
                code: 0,
                stdout: reply,
                stderr: String::new(),
            }
        } else {
            aterm_agent::supervise::run::CtlReply {
                code: 1,
                stdout: String::new(),
                stderr: format!("aterm-ctl: {reply}"),
            }
        })
    }
}

fn host(t: Terminal, ledger: TurnLedger) -> Host {
    let handle = crate::session_store::test_handle(0);
    *handle.ctx.turns.lock().unwrap() = ledger;
    Host {
        ctx: handle.ctx,
        term: Arc::new(Mutex::new(t)),
        requests: Vec::new(),
    }
}

fn report(h: &mut Host) -> aterm_agent::supervise::Report {
    let mut session = aterm_agent::supervise::run::Session::new(h, None);
    session
        .report(&aterm_agent::supervise::ReportOpts::default())
        .expect("report")
}

fn is_chrome(row: &str) -> bool {
    row.starts_with('─') || row.trim() == "❯" || row.contains("? for shortcuts")
}

/// Design test 13, end to end: the app answers a turn with N rows, the
/// session is handed to a new process, the app says M more — and `aterm
/// drive report` on the new process returns the turn's row and all N+M rows,
/// `complete=1`, no chrome; `history` shows the turn `carried=1`.
#[test]
fn a_report_across_a_handoff_has_every_row_and_no_chrome() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = env_guard();
    let (n, m) = (60, 40);
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, n);
    let minted = crate::control::turn_ids_minted() + 100;
    let carry = outgoing(&term, &turns, Some(minted));
    let staged = stage_carry_handoff("carry-e2e", &carry);
    let ((proof, _ready, _), mut adopted) = as_successor(&staged, |_| {
        child_proof_from(take_incoming()).expect("the successor adopts and proves")
    });
    assert_eq!(proof, staged.expected, "the carry costs the proof nothing");
    assert!(
        ctl_files(&staged).is_empty(),
        "the receiver deleted the sidecar"
    );
    assert!(
        crate::control::turn_ids_minted() >= minted,
        "turn ids continue above the carried count"
    );
    let control = adopted[0].control.take().expect("the control carry came");
    let (mut t, ledger, outcome) = adopt(&carry.screens[0], Some(control));
    assert_eq!(outcome, Some(AltArchiveImport::Exact));

    // The app repaints what it showed, then goes on.
    t.process(&app.frame(ROWS, COLS));
    app.say(&mut t, numbered(n + 1..=n + m));

    let mut h = host(t, ledger);
    let history = crate::control::cmd_history(&h.ctx, "");
    assert!(
        history.contains(" arch=") && history.contains(" carried=1 text=count%20to%2060"),
        "{history}"
    );
    let r = report(&mut h);
    let mut want = vec![format!("❯ {TURN_TEXT}")];
    want.extend(numbered(1..=n + m));
    assert_eq!(
        r.header().split(' ').nth(1),
        Some("complete=1"),
        "{}",
        r.header()
    );
    assert_eq!(r.turn, Some(41));
    assert_eq!(r.rows, want, "{}", r.render());
    assert!(r.archived > 0 && r.screen > 0, "{}", r.header());
    assert!(!r.rows.iter().any(|row| is_chrome(row)));
    staged.teardown();
}

/// H2, the resize variant: the window's first resize after the adoption is a
/// real resize, so the report says `archive-gap` — and still has every row
/// (duplicates allowed, never a loss) and no chrome.
#[test]
fn a_report_across_a_handoff_and_a_resize_says_gap_and_loses_nothing() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = env_guard();
    let (n, m) = (60, 40);
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, n);
    let carry = outgoing(&term, &turns, None);
    let staged = stage_carry_handoff("carry-e2e-resize", &carry);
    let mut adopted = as_successor(&staged, |_| take_incoming().adopted);
    assert_eq!(adopted.len(), 1);
    let (mut t, ledger, _) = adopt(&carry.screens[0], adopted[0].control.take());
    t.process(&app.frame(ROWS, COLS));
    t.resize(ROWS - 1, COLS);
    t.process(&app.frame(ROWS - 1, COLS));
    app.say(&mut t, numbered(n + 1..=n + m));

    let mut h = host(t, ledger);
    let r = report(&mut h);
    assert_eq!(
        r.reasons,
        vec![aterm_agent::supervise::Reason::ArchiveGap],
        "{}",
        r.header()
    );
    let mut want = vec![format!("❯ {TURN_TEXT}")];
    want.extend(numbered(1..=n + m));
    let mut rest = r.rows.iter();
    for row in &want {
        assert!(
            rest.any(|got| got == row),
            "{row} missing, or out of order:\n{}",
            r.render()
        );
    }
    assert!(!r.rows.iter().any(|row| is_chrome(row)), "{}", r.render());
    staged.teardown();
}

/// Design test 7: the round trip keeps the ledger and the archive's tail —
/// `offscreen since=<old origin>:k` reads the rows it read before the
/// handoff, and `history since=<old id>` goes on from there.
#[test]
fn the_carry_round_trips_the_ledger_and_the_archive_tail() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = env_guard();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 60);
    let mark = turns.lock().unwrap().records().last().unwrap().arch;
    let since = format!("since={}:{} max=5000", mark.origin, mark.last);
    let before = crate::control::cmd_offscreen(&term, &since);
    let carry = outgoing(&term, &turns, None);
    let staged = stage_carry_handoff("carry-roundtrip", &carry);
    let mut adopted = as_successor(&staged, |_| take_incoming().adopted);
    let (t, ledger, _) = adopt(&carry.screens[0], adopted[0].control.take());
    let h = host(t, ledger);
    let after = crate::control::cmd_offscreen(&h.term, &since);
    // The header's seq= is the engine's own content counter; everything else
    // — origin, indices, rows, gaps — is the same archive.
    let strip_seq = |s: &str| {
        s.split_whitespace()
            .filter(|w| !w.starts_with("seq=") && !w.starts_with("epoch="))
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(strip_seq(&after), strip_seq(&before));
    assert!(after.contains(&format!("origin={ORIGIN}")));
    let history = crate::control::cmd_history(&h.ctx, "since=40");
    assert!(history.starts_with("OK 1\nturn 41 "), "{history}");
    assert_eq!(crate::control::cmd_history(&h.ctx, "since=41"), "OK 0\n");
    staged.teardown();
}

/// Design test 8: a sidecar that is missing, cut short, of another sha, or
/// bigger than a sidecar may be, costs the carry and nothing else — the
/// session adopts, and the proof checks out.
#[test]
fn a_bad_sidecar_only_drops_the_carry() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = env_guard();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 30);
    let carry = outgoing(&term, &turns, None);
    type Spoil = fn(&StagedHandoff, &std::path::Path);
    let spoils: [(&str, Spoil); 6] = [
        ("missing", |_, p| std::fs::remove_file(p).unwrap()),
        // The manifest's stamp names more than a sidecar may hold: dropped
        // unread (the manifest bytes are hashed by no one, so rewriting them
        // is what a different sender would have written).
        ("stamped-oversized", |staged, _| {
            let body = std::fs::read_to_string(&staged.manifest_path).unwrap();
            let (nonce, toml) = body.split_once('\n').unwrap();
            let mut m = SessionHandoff::from_toml(toml).unwrap();
            let stamp = m.sessions[0].control.take().unwrap();
            let sha = stamp.split_once(' ').unwrap().1.to_string();
            m.sessions[0].control = Some(format!("{} {sha}", handoff_carry::MAX_SIDECAR_BYTES + 1));
            std::fs::write(
                &staged.manifest_path,
                format!("{nonce}\n{}", m.to_toml().unwrap()),
            )
            .unwrap();
        }),
        ("truncated", |_, p| {
            let b = std::fs::read(p).unwrap();
            std::fs::write(p, &b[..b.len() / 2]).unwrap();
        }),
        ("bad-sha", |_, p| {
            let mut b = std::fs::read(p).unwrap();
            let at = b.len() / 2;
            b[at] ^= 0x20;
            std::fs::write(p, b).unwrap();
        }),
        ("grown", |_, p| {
            let mut b = std::fs::read(p).unwrap();
            b.extend_from_slice(b"   ");
            std::fs::write(p, b).unwrap();
        }),
        ("oversized", |_, p| {
            let big = vec![b' '; handoff_carry::MAX_SIDECAR_BYTES as usize + 1];
            std::fs::write(p, big).unwrap();
        }),
    ];
    for (what, spoil) in spoils {
        let staged = stage_carry_handoff(&format!("carry-bad-{what}"), &carry);
        let files = ctl_files(&staged);
        assert_eq!(files.len(), 1, "{what}");
        spoil(&staged, &files[0]);
        let ((proof, _ready, _), adopted) = as_successor(&staged, |_| {
            child_proof_from(take_incoming()).expect("adoption still succeeds")
        });
        assert_eq!(proof, staged.expected, "{what}: the proof still checks out");
        assert_eq!(adopted.len(), 1, "{what}");
        assert!(adopted[0].control.is_none(), "{what}: no carry");
        assert!(
            adopted[0].checkpoint.is_some(),
            "{what}: the screen still came"
        );
        assert!(ctl_files(&staged).is_empty(), "{what}: nothing left behind");
        staged.teardown();
    }
}

/// Design test 9: the sidecar is in neither proof digest — the parent's
/// screen commitment is the same with and without it, and so is what the
/// child computes from the bytes it reads.
#[test]
fn the_proof_digests_do_not_see_the_sidecar() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = env_guard();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 30);
    let with = outgoing(&term, &turns, Some(7));
    let without = StageCarry {
        screens: with.screens.clone(),
        controls: Vec::new(),
        next_turn_id: None,
    };
    let mut child = Vec::new();
    for (label, carry) in [
        ("carry-digest-with", &with),
        ("carry-digest-without", &without),
    ] {
        let staged = stage_carry_handoff(label, carry);
        let incoming = as_successor(&staged, |_| take_incoming());
        assert_eq!(incoming.adopted.len(), 1, "{label}");
        assert_eq!(
            incoming.screen_digest,
            Some(staged.screen_digest),
            "{label}"
        );
        child.push((staged.screen_digest, incoming.layout_digest));
        staged.teardown();
    }
    assert_eq!(child[0], child[1], "with and without the sidecar");
}

/// Design test 14 — THE ROLLBACK LANE, a real handoff: this (new) sender,
/// sidecars and turn-id count included, hands off to a receiver built before
/// the carry. It adopts every session, proves exactly what the parent
/// expects, and never opens a `.ctl` — which the sender then unlinks once the
/// proof has checked out, as `run_handoff_decision` does.
#[test]
fn a_new_sender_hands_off_to_a_receiver_built_before_the_carry() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = env_guard();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 30);
    let carry = outgoing(&term, &turns, Some(crate::control::turn_ids_minted() + 5));
    let staged = stage_carry_handoff("carry-rollback", &carry);
    let published = std::fs::read_to_string(&staged.manifest_path).unwrap();
    assert!(published.contains("next_turn_id = ") && published.contains("control = \""));
    let (proof, adopted_ids, adopted) = as_successor(&staged, |ready_read| {
        let ((proof, ready, ids), adopted) =
            child_proof_from(take_incoming_as(ReceiverShape::PreCarry))
                .expect("the older receiver adopts and proves");
        assert!(ready.signal_proof(proof), "ProofReady is published");
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
        (proof, ids, adopted)
    });
    assert_eq!(adopted_ids.len(), 1);
    assert_eq!(proof, staged.expected, "the proof the parent expects");
    assert!(
        staged
            .expected
            .commit_wire_matches(&staged.expected.to_commit_wire()),
        "the Commit the parent would send is the one the child accepts"
    );
    assert!(
        adopted[0].control.is_none(),
        "the older receiver reads no carry"
    );
    assert_eq!(
        ctl_files(&staged).len(),
        1,
        "and leaves the sidecar where it was"
    );
    retire_outgoing_controls(&staged.nonce);
    assert!(
        ctl_files(&staged).is_empty(),
        "the sender retires it after the proof"
    );
    staged.teardown();
}

/// Design test 10: an old-shaped record parses a manifest carrying `control`
/// and `next_turn_id`; this build parses an old manifest.
#[test]
fn manifests_cross_between_the_two_shapes_both_ways() {
    let screen = ScreenCarry {
        schema: ScreenCarry::SCHEMA,
        meta: "{\"rows\":24}".to_string(),
        grid_file: "/x/seamless-1-n.s0.grid".to_string(),
        alt_grid_file: None,
    };
    let new = SessionHandoff {
        schema: SessionHandoff::SCHEMA,
        window: None,
        connections: Vec::new(),
        next_turn_id: Some(1234),
        sessions: vec![SessionRecord {
            local_id: 0,
            sid: "s-0".to_string(),
            parent: None,
            state: "alive".to_string(),
            title: "zsh".to_string(),
            screen: Some(screen.clone()),
            user_title: Some("worker".to_string()),
            description: None,
            icon: None,
            role: None,
            attention: None,
            control: Some(handoff_carry::stamp(b"{}")),
        }],
    };
    let wire = new.to_toml().unwrap();
    assert!(new.roundtrips());
    let old = PreCarryHandoff::from_toml(&wire).expect("an old reader skips the new keys");
    assert_eq!(old.sessions[0].screen.as_ref(), Some(&screen));
    assert_eq!(old.sessions[0].user_title.as_deref(), Some("worker"));

    let old_wire = aterm_toml::to_string(&old).unwrap();
    assert!(!old_wire.contains("control") && !old_wire.contains("next_turn_id"));
    let read = SessionHandoff::from_toml(&old_wire).expect("this build reads an old manifest");
    assert_eq!(read.next_turn_id, None);
    assert_eq!(read.sessions[0].control, None);
    assert_eq!(read.sessions[0].screen.as_ref(), Some(&screen));
    assert_eq!(
        pre_carry_parse(&wire).unwrap(),
        read,
        "the two readers agree on the rest"
    );
}

/// M3: at startup the files a dead sender left (`seamless-<dead pid>-…`) are
/// retired; a live pid's — ours, a handoff in flight — and anything else in
/// the directory are not.
#[test]
fn the_startup_sweep_retires_only_a_dead_senders_files() {
    let dir = std::env::temp_dir().join(format!("aterm-carry-sweep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let own = std::process::id();
    let dead = [
        "seamless-9999-aaaa.toml",
        "seamless-9999-aaaa.layout.toml",
        "seamless-9999-aaaa.s0.grid",
        "seamless-9999-aaaa.s0.ctl",
    ];
    let kept = [
        "seamless-4242-bbbb.toml".to_string(),
        "seamless-4242-bbbb.s1.ctl".to_string(),
        format!("seamless-{own}-cccc.s0.ctl"),
        "seamless-x9-dddd.toml".to_string(),
        "seamless-9999".to_string(),
        "aterm-9999.sock".to_string(),
    ];
    for name in dead.iter().copied().chain(kept.iter().map(String::as_str)) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
    std::fs::create_dir_all(dir.join("seamless-9999-dir")).unwrap();
    sweep_dead_handoff_leftovers_in(&dir, &|pid| pid == 4242);
    for name in dead {
        assert!(!dir.join(name).exists(), "{name} retired");
    }
    for name in &kept {
        assert!(dir.join(name).exists(), "{name} kept");
    }
    assert!(dir.join("seamless-9999-dir").is_dir());

    // And the production predicate: a reaped child's pid is dead, ours is not.
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(!crate::control_auth::pid_alive(pid));
    assert!(crate::control_auth::pid_alive(own));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sender's two retirements: a rollback's `discard_outgoing` takes the
/// sidecars with everything else, and the post-proof `retire_outgoing_controls`
/// takes the sidecars alone — of this attempt only.
#[test]
fn a_senders_retirements_take_its_sidecars_and_nothing_else() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore_xdg = RestoreVar::new("XDG_RUNTIME_DIR");
    let tmp = std::env::temp_dir().join(format!("aterm-carry-retire-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    aterm_log::env::set("XDG_RUNTIME_DIR", &tmp);
    let dir = crate::control_auth::socket_dir().expect("scratch control dir");
    std::fs::create_dir_all(&dir).unwrap();
    let pid = std::process::id();
    let path = |name: &str| dir.join(name.replace("PID", &pid.to_string()));
    for name in [
        "seamless-PID-aaaa.toml",
        "seamless-PID-aaaa.s0.grid",
        "seamless-PID-aaaa.s0.ctl",
        "seamless-PID-aaaa.s7.ctl",
        "seamless-PID-bbbb.s0.ctl",
    ] {
        std::fs::write(path(name), b"x").unwrap();
    }
    retire_outgoing_controls("aaaa");
    assert!(!path("seamless-PID-aaaa.s0.ctl").exists());
    assert!(!path("seamless-PID-aaaa.s7.ctl").exists());
    assert!(path("seamless-PID-aaaa.toml").exists(), "only sidecars");
    assert!(path("seamless-PID-aaaa.s0.grid").exists(), "only sidecars");
    assert!(
        path("seamless-PID-bbbb.s0.ctl").exists(),
        "only this attempt's"
    );
    std::fs::write(path("seamless-PID-aaaa.s3.ctl"), b"x").unwrap();
    discard_outgoing("aaaa");
    assert!(
        !path("seamless-PID-aaaa.s3.ctl").exists(),
        "rollback takes them too"
    );
    assert!(!path("seamless-PID-aaaa.toml").exists());
    assert!(path("seamless-PID-bbbb.s0.ctl").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `write_outgoing` never writes a sidecar through what is already at its
/// path — a planted symlink included — and never fails the handoff for it.
#[test]
fn a_sidecar_is_never_written_through_a_planted_path() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = env_guard();
    let target = std::env::temp_dir().join(format!("aterm-carry-target-{}", std::process::id()));
    std::fs::write(&target, b"untouched").unwrap();
    let p = std::env::temp_dir().join(format!("aterm-carry-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    std::os::unix::fs::symlink(&target, &p).unwrap();
    assert!(
        write_private_new(&p, b"secret").is_none(),
        "O_EXCL|O_NOFOLLOW"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    let _ = std::fs::remove_file(&p);
    let fresh = std::env::temp_dir().join(format!("aterm-carry-fresh-{}", std::process::id()));
    let _ = std::fs::remove_file(&fresh);
    assert!(write_private_new(&fresh, b"ok").is_some());
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let _ = std::fs::remove_file(&fresh);
    let _ = std::fs::remove_file(&target);
}

/// A sidecar whose stamp matches but which names a turn id past any a
/// manifest can carry (a TOML integer is an `i64`) does not use up the
/// turn-id count: that record is left out, the rest of the carry comes, and
/// the next `turn` id is still one the counter can mint. (u64::MAX here made
/// the next `turn` panic on overflow in a debug build and wrap to 0 in a
/// release one, breaking the rising ids `history since=` and `ERR busy turn=`
/// rely on.)
#[test]
fn a_carried_turn_id_past_the_manifest_range_is_left_out() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = env_guard();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 30);
    let mut carry = outgoing(&term, &turns, None);
    let text = String::from_utf8(carry.controls[0].1.clone()).unwrap();
    assert!(text.contains("\"id\":41"), "{text}");
    carry.controls[0].1 = text
        .replacen("\"id\":41", &format!("\"id\":{}", u64::MAX), 1)
        .into_bytes();
    let staged = stage_carry_handoff("carry-turn-id-range", &carry);
    let mut incoming = as_successor(&staged, |_| take_incoming());
    assert_eq!(incoming.adopted.len(), 1);
    let minted = crate::control::turn_ids_minted();
    assert!(
        minted <= handoff_carry::MAX_TURN_ID,
        "the next `turn` id overflows: NEXT_TURN_ID = {minted}"
    );
    let control = incoming.adopted[0]
        .control
        .take()
        .expect("the rest of the carry still came");
    assert_eq!(
        control.turns.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![40],
        "only the out-of-range record is left out"
    );
    assert!(control.archive.is_some());
    staged.teardown();
}

/// A FIFO swapped in at a sidecar's path (a same-user process racing the
/// handoff) never stalls the successor: the read opens without blocking,
/// finds no regular file, and drops that carry — the adoption and its proof
/// go on. Before, `open(2)` blocked until the parent's ready deadline and the
/// update rolled back.
#[test]
fn a_fifo_at_the_sidecar_path_does_not_stall_the_adoption() {
    use std::os::unix::ffi::OsStrExt as _;
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = env_guard();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 30);
    let carry = outgoing(&term, &turns, None);
    let staged = stage_carry_handoff("carry-fifo", &carry);
    let files = ctl_files(&staged);
    assert_eq!(files.len(), 1);
    let fifo = files[0].clone();
    std::fs::remove_file(&fifo).unwrap();
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a NUL-terminated path; mkfifo only creates the node.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let outcome = as_successor(&staged, |_| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child_proof_from(take_incoming()));
        });
        let got = rx.recv_timeout(std::time::Duration::from_secs(3));
        if got.is_err() {
            // Release the open blocked on the FIFO so the thread can finish.
            use std::os::unix::fs::OpenOptionsExt as _;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo);
            let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
        }
        got
    });
    let Ok(Some(((proof, _ready, _), adopted))) = outcome else {
        panic!("take_incoming did not return within 3 s (blocked opening the FIFO)");
    };
    assert_eq!(proof, staged.expected, "the proof still checks out");
    assert_eq!(adopted.len(), 1);
    assert!(adopted[0].control.is_none(), "no carry from a FIFO");
    assert!(adopted[0].checkpoint.is_some(), "the screen still came");
    assert!(!fifo.exists(), "the FIFO is consumed like any sidecar");
    staged.teardown();
}

// ------------------------------------------------- round-10 review, continuity

/// The freeze and the worker, with the freeze's `differ` choice: past half
/// its budget the freeze leaves the differ's state out.
fn carried_with(
    term: &Arc<Mutex<Terminal>>,
    turns: &Arc<Mutex<TurnLedger>>,
    differ: bool,
) -> (TerminalCheckpoint, ControlCarry) {
    let (checkpoint, source) = {
        let guard = term.lock().unwrap();
        (
            guard.checkpoint_carry(256).expect("parser is Ground"),
            handoff_carry::capture_head(0, &guard, term, turns, differ),
        )
    };
    let bytes = handoff_carry::export(&[source]).remove(0).1;
    (checkpoint, handoff_carry::decode(&bytes).expect("decodes"))
}

/// The freeze had no time for the differ's state: the worker takes it (no
/// frame committed since), so at the same size a report across the handoff
/// is whole — every row once, `complete=1`, as the help says. (Review: the
/// adopting engine started a new baseline after a `restore` gap, and the
/// report said `archive-gap` with every row there.)
#[test]
fn a_report_is_whole_when_the_freeze_left_the_differ_state_out() {
    let (n, m) = (60, 40);
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, n);
    let (checkpoint, control) = carried_with(&term, &turns, false);
    let (mut t, ledger, outcome) = adopt(&checkpoint, Some(control));
    assert_eq!(outcome, Some(AltArchiveImport::Exact));
    t.process(&app.frame(ROWS, COLS));
    app.say(&mut t, numbered(n + 1..=n + m));
    let r = report(&mut host(t, ledger));
    let mut want = vec![format!("❯ {TURN_TEXT}")];
    want.extend(numbered(1..=n + m));
    assert_eq!(
        r.rows,
        want,
        "nothing lost, nothing repeated:\n{}",
        r.render()
    );
    assert_eq!(
        r.header().split(' ').nth(1),
        Some("complete=1"),
        "{}",
        r.header()
    );
}

/// A handoff that could not carry the ledger and the archive (no sidecar, a
/// bad one) starts both afresh, as the help now says: `history` is empty, so
/// `report` looks for the last `❯` row it can still see — none here, so
/// `marker-not-found` — and `--since` a mark from before the handoff says
/// `archive-reset`. (Review: the help said `archive-reset` for both.)
#[test]
fn a_report_after_a_dropped_carry_starts_afresh() {
    use aterm_agent::supervise::{Mark, Marker, Reason, ReportOpts};
    let (n, m) = (60, 40);
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, n);
    let mark = turns.lock().unwrap().records().last().unwrap().arch;
    let (checkpoint, _) = carried_with(&term, &turns, true);
    let (mut t, _, outcome) = adopt(&checkpoint, None);
    assert_eq!(outcome, None);
    t.process(&app.frame(ROWS, COLS));
    app.say(&mut t, numbered(n + 1..=n + m));
    let mut h = host(t, handoff_carry::adopted_ledger(None));
    let r = report(&mut h);
    assert_eq!(r.marker, Marker::UserRow, "{}", r.header());
    assert_eq!(r.reasons, vec![Reason::MarkerNotFound], "{}", r.header());
    let mut session = aterm_agent::supervise::run::Session::new(&mut h, None);
    let r = session
        .report(&ReportOpts {
            since: Some(Mark {
                origin: Some(mark.origin),
                index: mark.last,
            }),
            ..ReportOpts::default()
        })
        .expect("report");
    assert!(r.reasons.contains(&Reason::ArchiveReset), "{}", r.header());
}

/// A `subscribe … events since-turn=<n>` resumed after a handoff that could
/// not carry the session's ledger is told the turns up to the carried count
/// are gone (`GAP … events-resync=`), as it is when records are evicted or
/// a carried ledger was halved. A ledger carried whole, with no turn in it,
/// tells nothing. (Review: the dropped ledger had no low-water, so the resume
/// said nothing and turns vanished silently.)
#[test]
fn a_since_turn_resume_after_a_dropped_ledger_is_told_of_the_loss() {
    let minted = crate::control::turn_ids_minted() + 50;
    // `take_incoming`: the manifest's `next_turn_id`.
    crate::control::raise_turn_ids(minted);
    let resume = |local: u64, ledger: TurnLedger| -> String {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(local);
        *h.ctx.turns.lock().unwrap() = ledger;
        let target: crate::subscribe::ResolvedTarget = (
            local,
            h.term.clone(),
            h.ctx.byte_fanout.clone(),
            h.ctx.turns.clone(),
            h.ctx.timeline.clone(),
        );
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let registry = crate::subscribe::new_registry();
        let mut sink: Vec<u8> = Vec::new();
        crate::subscribe::push_loop_with_peer_probe(
            &registry,
            &store,
            &[target],
            crate::subscribe::PushScopes {
                streams: crate::subscribe::TargetStreams {
                    events: true,
                    ..Default::default()
                },
                instance: crate::subscribe::InstanceStreams::default(),
                adopt: crate::subscribe::AdoptScope::none(),
            },
            crate::subscribe::PushOptions {
                since_turn: Some(minted - 10),
                ..Default::default()
            },
            &mut sink,
            || true,
        );
        String::from_utf8_lossy(&sink).into_owned()
    };
    // The control: a carried ledger halved to its newest record.
    let mut halved = ControlCarry {
        turns: vec![TurnRecord {
            id: minted,
            started_ms: 1,
            dur_ms: 1,
            submitted: true,
            status: "settled",
            text: "t".to_string(),
            screen_hash: 1,
            seq: 1,
            arch: ArchMark::default(),
            carried: true,
        }],
        unheld_below: minted,
        archive: None,
    };
    let told = resume(8, handoff_carry::adopted_ledger(Some(&mut halved)));
    assert!(
        told.contains(&format!("GAP 8 events-resync={minted}")),
        "{told:?}"
    );
    // The ledger dropped whole: turns minted-9..=minted are just as gone.
    let out = resume(9, handoff_carry::adopted_ledger(None));
    assert!(
        out.contains("GAP 9 events-resync="),
        "turns {}..={minted} were lost with the ledger: {out:?}",
        minted - 9
    );
    // Carried whole, and empty: nothing was lost, nothing is said.
    let quiet = resume(
        10,
        handoff_carry::adopted_ledger(Some(&mut ControlCarry::default())),
    );
    assert!(!quiet.contains("GAP"), "{quiet:?}");
}

impl FakeApp {
    /// A frame showing the doc from row `top` (someone scrolled the app back).
    fn frame_from(&self, top: usize, rows: u16, cols: u16) -> Vec<u8> {
        let t = usize::from(rows) - 5;
        let rule = "─".repeat(usize::from(cols) - 1);
        let mut screen: Vec<String> = self.doc[top..top + t].to_vec();
        screen.extend([
            String::new(),
            rule.clone(),
            "❯".to_string(),
            rule,
            "  ? for shortcuts".to_string(),
        ]);
        let mut v = b"\x1b[?2026h\x1b[?25l".to_vec();
        for (r, text) in screen.iter().enumerate() {
            v.extend_from_slice(format!("\x1b[{};1H\x1b[2K{text}", r + 1).as_bytes());
        }
        v.extend_from_slice(b"\x1b[?25h\x1b[?2026l");
        v
    }
}

/// The first turn driven after an update, at the same size: someone scrolls
/// the worker's app back past the carried tail's first row (the turns' marks
/// are recent; 120 rows before them were typed with no `turn`) and down
/// again, mid-turn. The carry reaches as far back as the differ can point, so
/// the adopted engine recognizes the re-shown rows as the old process would
/// have: `offscreen since=<turn mark>` reads the same rows from both.
/// (Review: the adopted engine archived 75 of them again, as new rows, with
/// no gap and `lost=0`.)
#[test]
fn a_scroll_back_past_the_turns_marks_after_a_handoff_archives_nothing_twice() {
    let mut app = FakeApp::new();
    let mut pre = live_engine(ORIGIN);
    pre.process(b"\x1b[?1049h");
    pre.process(&app.frame(ROWS, COLS));
    app.say(
        &mut pre,
        (0..120).map(|i| format!("⏺ earlier human chat {i:04}")),
    );
    let arch41 = ArchMark::of(&pre);
    app.say(&mut pre, [format!("❯ {TURN_TEXT}")]);
    app.say(&mut pre, numbered(1..=20));
    let mut l = TurnLedger::default();
    l.push(TurnRecord {
        id: 41,
        started_ms: 100,
        dur_ms: 2000,
        submitted: true,
        status: "settled",
        text: TURN_TEXT.to_string(),
        screen_hash: 2,
        seq: pre.content_seq(),
        arch: arch41,
        carried: false,
    });
    let (term, turns) = (Arc::new(Mutex::new(pre)), Arc::new(Mutex::new(l)));
    let (checkpoint, control) = carried_with(&term, &turns, true);
    let first_carried = control.archive.as_ref().unwrap().first;
    assert!(
        first_carried < arch41.last + 1,
        "the carry reaches past the turn's mark"
    );
    let (mut t, _, outcome) = adopt(&checkpoint, Some(control));
    assert_eq!(outcome, Some(AltArchiveImport::Exact));
    let mut old = Arc::try_unwrap(term).ok().unwrap().into_inner().unwrap();
    // Turn 42, driven after the update.
    let arch = ArchMark::of(&t);
    assert_eq!(arch, ArchMark::of(&old));
    let mut both = |bytes: &[u8]| {
        old.process(bytes);
        t.process(bytes);
    };
    let mut lines = vec!["❯ now the next step".to_string()];
    lines.extend((1..=40).map(|i| format!("⏺ next-step row {i:02}")));
    for l in lines {
        app.doc.push(l);
        both(&app.frame(ROWS, COLS));
    }
    // Back 75 rows (past the turns' marks), three a frame, and down again.
    let top = app.doc.len() - (usize::from(ROWS) - 5);
    let mut views: Vec<usize> = (1..=25).map(|k| top - 3 * k).collect();
    views.extend((0..25).rev().map(|k| top - 3 * k));
    for v in views {
        both(&app.frame_from(v, ROWS, COLS));
    }
    for i in 41..=45 {
        app.doc.push(format!("⏺ next-step row {i:02}"));
        both(&app.frame(ROWS, COLS));
    }
    let since = format!("since={}:{} max=5000", arch.origin, arch.last);
    let strip = |r: String| -> Vec<String> {
        r.lines()
            .map(|line| {
                line.split(' ')
                    .filter(|w| !w.starts_with("seq="))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    let o = strip(crate::control::cmd_offscreen(
        &Arc::new(Mutex::new(old)),
        &since,
    ));
    let n = strip(crate::control::cmd_offscreen(
        &Arc::new(Mutex::new(t)),
        &since,
    ));
    assert_eq!(
        n,
        o,
        "`offscreen {since}`: the adopted engine archived {} rows the old one recognized as re-shown",
        n.len().saturating_sub(o.len())
    );
    assert!(o[0].contains(" lost=0 breaks=0 "), "{}", o[0]);
}
