// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CONTROL CARRY across a real handoff: `write_outgoing` with sidecars,
//! the successor's `take_incoming`, the adoption proof, and what drivers read
//! afterwards — `history`, `offscreen`, and the harness's report reader
//! (`Session::report`) itself. Plus
//! the rollback lane: the same new sender handing off to a receiver built
//! BEFORE the carry, proof and all.

use std::sync::{Arc, Mutex, PoisonError};

use aterm_core::terminal::{AltArchiveImport, Terminal, TerminalCheckpoint};

use super::tests::{
    ENV_LOCK, ScratchEnv, StageCarry, StagedHandoff, child_proof_from, pipe_pair, restage_session,
    restore_env, stage_carry_handoff, stage_history_handoff, stage_ladder_handoff,
};
use super::*;
use crate::handoff_carry::{self, ControlCarry};
use crate::handoff_history::tests::history as shell_history;
use crate::session_store::SessionRecord;
use crate::turn_ledger::{ArchMark, TurnBy, TurnLedger, TurnRecord};

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
        // That build wrote no number: its shells predate the healing sessions.
        outgoing_build: None,
        held: Vec::new(),
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
                // That build had no question policy to carry.
                questions: None,
                autopilot: None,
                land_at: None,
                control: None,
                frozen_path: false,
                identity: None,
                topics: Vec::new(),
                fg_holder: None,
                rekey: false,
                loader: false,
                history: None,
                history_dropped: 0,
                history_withheld: false,
                history_lost: 0,
                hold: None,
                more_holds: Vec::new(),
                supervisor: None,
                claim_known: false,
                attention_owners: Vec::new(),
                viewport_from_bottom: None,
                render_debt: None,
                render_debt_cut: 0,
                fabric: Default::default(),
                timeline_id: 0,
                log_ids: None,
                log_seq: 0,
                journal_key: None,
                task: None,
                status_seen: None,
            })
            .collect(),
        roster_seq: None,
        fabric_attached: Vec::new(),
        fleet_told: Vec::new(),
        mirror_dropped: Vec::new(),
        mirror_served: false,
        machine_names: Vec::new(),
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
        by: TurnBy::Ctl,
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
        by: TurnBy::Ctl,
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
            handoff_carry::capture_head(0, &guard, term, turns, None, true),
        )
    };
    StageCarry {
        screens: vec![checkpoint],
        controls: handoff_carry::export(&[source], crate::control::turn_ids_for_handoff()),
        next_turn_id,
        fg_holders: Vec::new(),
        render_debts: Vec::new(),
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

fn this_build() -> Option<String> {
    Some(encode_target_identity(
        crate::build_info::build_number()
            .parse::<u64>()
            .unwrap_or(0),
        crate::build_info::git_commit(),
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

// --------------------------------------- the report reader (`Session::report`)

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
/// session is handed to a new process, the app says M more — and the report
/// reader (`Session::report`) on the new process returns the turn's row and all N+M rows,
/// `complete=1`, no chrome; `history` shows the turn `carried=1`.
#[test]
fn a_report_across_a_handoff_has_every_row_and_no_chrome() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
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
    // Past it by one overlap reserve (round eight of the update audit,
    // finding 33): a producer that carried no reserve (0.98) went on minting
    // in the overlap, and those ids must not be minted again here.
    assert!(
        crate::control::turn_ids_minted() >= minted + crate::control::TURN_ID_RESERVE,
        "turn ids continue above the carried count and its overlap"
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
    let _restore = restore_env();
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

/// THE SESSION LOG CROSSES THE REAL FILES (phase 2A): the worker's export
/// for a successor that reads it puts the log in the sidecar, the manifest
/// record carries its counts (`log_ids`, `log_seq`), `take_incoming` reads
/// both back (the sidecar off its file, length and sha checked; the counts
/// into `Adopted::log`), and the product's own adoption (`spawn::adopted_log`
/// taking the lines off the sidecar, then `spawn::mark_handoff_gap`)
/// continues it: the same records, the next epoch, the overlap's seqs above
/// the manifest's count. NEGATIVE CONTROL: the same adoption with the
/// sidecar's member gone starts over, `gap why=handoff`, its seq still above
/// the count.
#[test]
fn the_session_log_crosses_the_real_files() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 30);
    let timeline = Arc::new(Mutex::new(
        crate::session_timeline::SessionTimeline::default(),
    ));
    {
        let mut g = timeline.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, now);
        for _ in 0..40 {
            g.slog_mut().human_input(true, now);
        }
    }
    let before: Vec<String> = timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).unwrap())
        .collect();
    // The manifest's draw, as `SessionHandoff::from_store` takes it.
    let (ids, seq) = {
        let mut g = timeline.lock().unwrap();
        (g.handoff_log_ids(), g.handoff_log_seq())
    };
    let (checkpoint, source) = {
        let guard = term.lock().unwrap();
        (
            guard.checkpoint_carry(256).expect("parser is Ground"),
            handoff_carry::capture_head(0, &guard, &term, &turns, Some(&timeline), true),
        )
    };
    let carry = StageCarry {
        screens: vec![checkpoint],
        controls: handoff_carry::export_for_target(
            &[source],
            crate::control::turn_ids_for_handoff(),
            crate::running_build_number(),
        ),
        next_turn_id: None,
        fg_holders: Vec::new(),
        render_debts: Vec::new(),
    };
    let stamp = |m: &mut SessionHandoff, _: &std::path::Path, _: &str| {
        m.sessions[0].log_ids = Some(ids.clone());
        m.sessions[0].log_seq = seq;
    };
    let staged = stage_history_handoff("log-roundtrip", &carry, &stamp);
    let mut adopted = as_successor(&staged, |_| take_incoming().adopted);
    let a = &mut adopted[0];
    assert_eq!(
        (a.log.ids, a.log.seq),
        (crate::session_log::LogCarry::parse(&ids), seq),
        "the record's counts reach the adoption"
    );
    assert!(
        a.control.as_ref().is_some_and(|c| c.log.is_some()),
        "the sidecar crossed with the log"
    );
    let adopt = |log: crate::session_log::LogAdopt| -> Vec<aterm_slog::Rec> {
        let successor = crate::session_store::test_handle(0);
        crate::spawn::mark_handoff_gap(
            &successor.ctx.timeline,
            &successor.ctx.cast,
            &successor.ctx.temporal,
            None,
            0,
            log,
            false,
        );
        let g = successor.ctx.timeline.lock().unwrap();
        g.slog().log().window().cloned().collect()
    };
    let stripped = crate::spawn::adopted_log(&a.log, None);
    let recs = adopt(crate::spawn::adopted_log(&a.log, a.control.as_mut()));
    assert!(
        a.control.as_ref().is_some_and(|c| c.log.is_none()),
        "the lines were taken off the sidecar"
    );
    let after: Vec<String> = recs
        .iter()
        .take(before.len())
        .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).unwrap())
        .collect();
    assert_eq!(after, before, "the same records");
    let mark = recs.last().unwrap();
    assert_eq!(mark.k, aterm_slog::Kind::Handoff);
    assert_eq!(mark.seq, seq + 1, "above the manifest's count");
    // NEGATIVE CONTROL: without the member, the log starts over.
    let fresh = adopt(stripped);
    assert_eq!(fresh.len(), 1, "{fresh:?}");
    assert_eq!(
        (fresh[0].k, fresh[0].seq),
        (aterm_slog::Kind::Gap, seq + 1),
        "a handoff gap above the count"
    );
    staged.teardown();
}

/// Design test 7: the round trip keeps the ledger and the archive's tail —
/// `offscreen since=<old origin>:k` reads the rows it read before the
/// handoff, and `history since=<old id>` goes on from there.
#[test]
fn the_carry_round_trips_the_ledger_and_the_archive_tail() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
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

/// Round seven, finding 24: a session carried with a repainted screen (the
/// producer lowered it to Sanitized or Repaint, or this receiver degraded
/// it) goes without the differ's state only. Its turn ledger and archive
/// rows are not about the screen: `history` and `offscreen` read on across
/// the update, where the whole carry used to be dropped unread.
#[test]
fn a_repainted_session_keeps_its_ledger_and_archive_rows() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 60);
    let mark = turns.lock().unwrap().records().last().unwrap().arch;
    let since = format!("since={}:{} max=5000", mark.origin, mark.last);
    let carry = outgoing(&term, &turns, None);
    let staged = stage_ladder_handoff("carry-repaint", &carry, &[0]);
    let mut adopted = as_successor(&staged, |_| take_incoming().adopted);
    let control = adopted[0]
        .control
        .take()
        .expect("the ledger and rows still come");
    let archive = control.archive.as_ref().expect("the archive came");
    assert!(archive.differ.is_none(), "the differ's state stays behind");
    assert!(!archive.rows.is_empty(), "the rows came");
    assert_eq!(control.turns.len(), 2, "the whole ledger came");
    let (t, ledger, _) = adopt(&carry.screens[0], Some(control));
    let h = host(t, ledger);
    let history = crate::control::cmd_history(&h.ctx, "since=40");
    assert!(history.starts_with("OK 1\nturn 41 "), "{history}");
    let after = crate::control::cmd_offscreen(&h.term, &since);
    assert!(after.contains(&format!("origin={ORIGIN}")), "{after}");
    assert!(ctl_files(&staged).is_empty(), "nothing left behind");
    staged.teardown();
}

/// Round eight, finding 73: a session this receiver degrades because its
/// screen meta did not parse adopts onto a blank engine — no checkpoint at
/// all, the one degrade that has none — and keeps its archive rows as well
/// as its ledger. The engine is put back by the product's own call
/// (`spawn::hydrate_adopted_engine`), which installed the control carry
/// only beside a restored screen and dropped it here: `history` named
/// `arch=<old origin>:<n>` marks that `offscreen` then read from the start
/// of a fresh archive, and the report reader lost the turn's rows.
#[test]
fn a_receiver_degraded_screen_without_a_meta_keeps_its_archive_rows() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 60);
    let mark = turns.lock().unwrap().records().last().unwrap().arch;
    let since = format!("since={}:{} max=5000", mark.origin, mark.last);
    let before = crate::control::cmd_offscreen(&term, &since);
    let carry = outgoing(&term, &turns, None);
    let mut staged = stage_carry_handoff("carry-nometa", &carry);
    restage_session(&mut staged, 0, &|_| "{".to_string(), &|grid| grid);
    let mut adopted = as_successor(&staged, |_| take_incoming().adopted);
    assert_eq!(adopted.len(), 1, "the session still adopts");
    assert!(adopted[0].repaint, "degraded to a repaint");
    assert!(adopted[0].checkpoint.is_none(), "no meta, no checkpoint");
    let control = adopted[0].control.take().expect("the carry still came");
    let rows = control.archive.as_ref().map_or(0, |a| a.rows.len());
    assert!(rows > 0, "the rows came");
    let successor = Arc::new(Mutex::new(live_engine(0xfeed)));
    crate::spawn::hydrate_adopted_engine(
        &successor,
        None,
        Some(control),
        None,
        None,
        0,
        &mut adopted[0].history,
    );
    let after = crate::control::cmd_offscreen(&successor, &since);
    assert!(after.contains(&format!("origin={ORIGIN}")), "{after}");
    let carried_rows = |reply: &str| -> Vec<String> {
        reply
            .lines()
            .filter(|l| l.contains("counted"))
            .map(str::to_string)
            .collect()
    };
    assert!(!carried_rows(&before).is_empty(), "{before}");
    assert_eq!(
        carried_rows(&after),
        carried_rows(&before),
        "the turn's rows read on"
    );
    staged.teardown();
}

/// Round seven, finding 44: every way a control carry is lost at adoption
/// has a cause, which `take_incoming` logs by session — a lost carry no
/// longer reads like a session nobody drove.
#[test]
fn a_lost_control_carry_names_its_cause() {
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 30);
    let bytes = outgoing(&term, &turns, None).controls.remove(0).1;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("s0.ctl");
    let take = |written: Option<&[u8]>, stamp: &str, budget: u64| {
        let _ = std::fs::remove_file(&path);
        if let Some(written) = written {
            std::fs::write(&path, written).unwrap();
        }
        let mut remaining = budget;
        take_control_sidecar(&path, dir.path(), stamp, &mut remaining).map(|_| ())
    };
    let stamp = handoff_carry::stamp(&bytes);
    let all = handoff_carry::MAX_AGGREGATE_BYTES;
    assert_eq!(take(Some(&bytes), &stamp, all), Ok(()));
    let mut flipped = bytes.clone();
    let at = flipped.len() / 2;
    flipped[at] ^= 0x20;
    let cause = take(Some(&flipped), &stamp, all).unwrap_err();
    assert!(cause.contains("sha"), "{cause}");
    assert!(take(None, &stamp, all).unwrap_err().contains("missing"));
    assert!(
        take(Some(&bytes), &stamp, 1)
            .unwrap_err()
            .contains("budget")
    );
    assert!(
        take(Some(&bytes), "12 nope", all)
            .unwrap_err()
            .contains("malformed")
    );
    let text = String::from_utf8(bytes.clone()).unwrap();
    let newer = text.replacen("\"version\":1", "\"version\":2", 1);
    assert_ne!(newer, text);
    let cause = take(
        Some(newer.as_bytes()),
        &handoff_carry::stamp(newer.as_bytes()),
        all,
    )
    .unwrap_err();
    assert!(cause.contains("version"), "{cause}");
    let junk = b"not json";
    let cause = take(Some(junk), &handoff_carry::stamp(junk), all).unwrap_err();
    assert!(cause.contains("JSON"), "{cause}");
}

/// Design test 8: a sidecar that is missing, cut short, of another sha, or
/// bigger than a sidecar may be, costs the carry and nothing else — the
/// session adopts, and the proof checks out.
#[test]
fn a_bad_sidecar_only_drops_the_carry() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
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
    let _restore = restore_env();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 30);
    let mut with = outgoing(&term, &turns, Some(7));
    // The foreground holder rides the record, and is in neither digest either.
    with.fg_holders = vec![(0, 4242)];
    // So does the render debt (R08).
    with.render_debts = vec![(0, aterm_core::terminal::RenderDebt::of_rows(-1, 0))];
    let without = StageCarry {
        screens: with.screens.clone(),
        controls: Vec::new(),
        next_turn_id: None,
        fg_holders: Vec::new(),
        render_debts: Vec::new(),
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
    let _restore = restore_env();
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
        repaint: false,
    };
    let new = SessionHandoff {
        schema: SessionHandoff::SCHEMA,
        window: None,
        connections: Vec::new(),
        next_turn_id: Some(1234),
        outgoing_build: Some(crate::running_build_number()),
        held: Vec::new(),
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
            // Set on purpose: a worker's question policy stays with it across
            // the update.
            questions: Some("recommended".to_string()),
            // Set on purpose too: an Off session stays Off across the update.
            autopilot: Some("off".to_string()),
            land_at: None,
            control: Some(handoff_carry::stamp(b"{}")),
            frozen_path: true,
            identity: Some("worker".to_string()),
            // NON-EMPTY on purpose: the topic set is consent, and this is the
            // roundtrip that proves a seamless update does not drop it.
            topics: vec!["build.failed head".to_string(), "sat-comp @42".to_string()],
            // The foreground handback's holder (2026-09-25): a scalar on the
            // record, so it crosses at every rung, and an old reader skips it.
            fg_holder: Some(4242),
            rekey: false,
            loader: false,
            history: None,
            history_dropped: 0,
            history_withheld: false,
            history_lost: 0,
            hold: None,
            more_holds: Vec::new(),
            supervisor: None,
            claim_known: false,
            attention_owners: Vec::new(),
            viewport_from_bottom: None,
            // The render debt the parent's engine owed the alternate screen
            // (R08): a record scalar, so an old reader skips it.
            render_debt: Some("-1,0".to_string()),
            render_debt_cut: 0,
            fabric: Default::default(),
            timeline_id: 0,
            log_ids: None,
            log_seq: 0,
            journal_key: None,
            task: None,
            status_seen: None,
        }],
        roster_seq: None,
        fabric_attached: Vec::new(),
        fleet_told: Vec::new(),
        mirror_dropped: Vec::new(),
        mirror_served: false,
        machine_names: Vec::new(),
    };
    let wire = new.to_toml().unwrap();
    assert!(new.roundtrips());
    let old = PreCarryHandoff::from_toml(&wire).expect("an old reader skips the new keys");
    assert_eq!(old.sessions[0].screen.as_ref(), Some(&screen));
    assert_eq!(old.sessions[0].user_title.as_deref(), Some("worker"));

    let old_wire = aterm_toml::to_string(&old).unwrap();
    assert!(!old_wire.contains("control") && !old_wire.contains("next_turn_id"));
    assert!(!old_wire.contains("outgoing_build") && !old_wire.contains("frozen_path"));
    assert!(
        wire.contains("identity = \"worker\"") && !old_wire.contains("identity"),
        "the identity label rides this build's wire and an old reader drops it"
    );
    assert!(
        wire.contains("fg_holder = 4242") && !old_wire.contains("fg_holder"),
        "the foreground holder rides this build's wire and an old reader drops it"
    );
    assert!(
        wire.contains("questions = \"recommended\"") && !old_wire.contains("questions"),
        "the question policy rides this build's wire and an old reader drops it"
    );
    assert!(
        wire.contains("autopilot = \"off\"") && !old_wire.contains("autopilot"),
        "the Autopilot word rides this build's wire and an old reader drops it"
    );
    assert!(
        wire.contains("render_debt = \"-1,0\"") && !old_wire.contains("render_debt"),
        "the render debt rides this build's wire and an old reader drops it"
    );
    // Every question-policy word — the priority words among them — rides
    // this build's wire as written.
    for word in crate::session_timeline::QUESTIONS_POLICY_WORDS {
        let mut one = new.clone();
        one.sessions[0].questions = Some(word.to_string());
        let back = SessionHandoff::from_toml(&one.to_toml().unwrap()).expect("its own wire");
        assert_eq!(back.sessions[0].questions.as_deref(), Some(word));
    }
    let read = SessionHandoff::from_toml(&old_wire).expect("this build reads an old manifest");
    assert_eq!(read.next_turn_id, None);
    assert_eq!(
        read.outgoing_build, None,
        "absent: the old build's wire predates"
    );
    assert!(!read.sessions[0].frozen_path);
    assert_eq!(
        read.sessions[0].identity, None,
        "dropped by the old build: the label is gone"
    );
    assert_eq!(read.sessions[0].control, None);
    assert_eq!(
        read.sessions[0].fg_holder, None,
        "an old manifest names no holder: the reader probes afresh"
    );
    assert_eq!(
        read.sessions[0].render_debt, None,
        "an old manifest names no debt: the successor owes the program nothing"
    );
    assert_eq!(read.sessions[0].screen.as_ref(), Some(&screen));
    assert_eq!(
        pre_carry_parse(&wire).unwrap(),
        read,
        "the two readers agree on the rest"
    );
}

/// THE RENDER DEBT crosses the handoff (R08), and only onto a screen it is
/// about: the successor's `Adopted` carries what the parent's record stamped
/// when the screen was carried onto the alternate screen, and nothing when the
/// record has no key (a producer before the field, or a session that owed
/// nothing), when the screen was carried blank on purpose (the program
/// repaints), or when the session is on the main screen. Seeded onto a
/// restored engine it is the engine's debt again, and the first full clear
/// (the program repainting) zeroes it as ever.
#[test]
fn the_render_debt_crosses_the_handoff_and_only_onto_a_carried_alt_screen() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let mut app = FakeApp::new();
    let (term, turns) = old_session(&mut app, 30);
    {
        let mut t = term.lock().unwrap();
        t.resize(ROWS - 1, COLS);
        t.process(b"\x1b[?1004h");
        t.resize(ROWS, COLS);
        assert_eq!(
            t.render_debt(),
            aterm_core::terminal::RenderDebt::of_rows(-1, 0),
            "the flap left a residue"
        );
    }
    let mut with = outgoing(&term, &turns, None);
    with.render_debts = vec![(0, aterm_core::terminal::RenderDebt::of_rows(-1, 0))];
    let without = StageCarry {
        screens: with.screens.clone(),
        controls: Vec::new(),
        next_turn_id: None,
        fg_holders: Vec::new(),
        render_debts: Vec::new(),
    };

    // The producer stamped it: the successor's adoption carries it.
    let staged = stage_carry_handoff("render-debt-with", &with);
    let adopted = as_successor(&staged, |_| take_incoming().adopted);
    assert_eq!(
        adopted[0].render_debt,
        Some(aterm_core::terminal::RenderDebt::of_rows(-1, 0))
    );
    let (mut t, _, _) = adopt(&with.screens[0], None);
    assert_eq!(
        t.render_debt(),
        aterm_core::terminal::RenderDebt::default(),
        "a restored engine starts with none"
    );
    t.seed_render_debt(adopted[0].render_debt.unwrap());
    assert_eq!(
        t.render_debt(),
        aterm_core::terminal::RenderDebt::of_rows(-1, 0),
        "seeded"
    );
    t.process(b"\x1b[2J");
    assert_eq!(
        t.render_debt(),
        aterm_core::terminal::RenderDebt::default(),
        "the first full clear resets it"
    );
    staged.teardown();

    // A record without the key: nothing is carried (a debt-free predecessor,
    // or one that predates the field).
    let staged = stage_carry_handoff("render-debt-without", &without);
    let adopted = as_successor(&staged, |_| take_incoming().adopted);
    assert_eq!(adopted[0].render_debt, None);
    staged.teardown();

    // NEGATIVE CONTROL: a screen carried blank on purpose (the program is
    // asked to repaint) has no screen the debt is about.
    let staged = stage_ladder_handoff("render-debt-repaint", &with, &[0]);
    let adopted = as_successor(&staged, |_| take_incoming().adopted);
    assert!(adopted[0].repaint);
    assert_eq!(adopted[0].render_debt, None);
    staged.teardown();

    // NEGATIVE CONTROL: the main screen owes nothing, whatever a record says.
    let mut shell = live_engine(ORIGIN);
    shell.process(b"$ ls\r\n");
    let main = StageCarry {
        screens: vec![shell.checkpoint_carry(256).expect("parser is Ground")],
        controls: Vec::new(),
        next_turn_id: None,
        fg_holders: Vec::new(),
        render_debts: vec![(0, aterm_core::terminal::RenderDebt::of_rows(-1, 0))],
    };
    let staged = stage_carry_handoff("render-debt-main", &main);
    let adopted = as_successor(&staged, |_| take_incoming().adopted);
    assert_eq!(adopted[0].render_debt, None);
    staged.teardown();
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
        // The restore machinery's hidden siblings of the layout: its lock, and a
        // temporary a crash left mid-publish.
        ".seamless-9999-aaaa.layout.toml.aterm-restore.lock",
        ".seamless-9999-aaaa.layout.toml.aterm-write-9999-1-0.tmp",
    ];
    let kept = [
        "seamless-4242-bbbb.toml".to_string(),
        "seamless-4242-bbbb.s1.ctl".to_string(),
        ".seamless-4242-bbbb.layout.toml.aterm-restore.lock".to_string(),
        format!("seamless-{own}-cccc.s0.ctl"),
        format!(".seamless-{own}-cccc.layout.toml.aterm-restore.lock"),
        "seamless-x9-dddd.toml".to_string(),
        "seamless-9999".to_string(),
        ".seamless-9999".to_string(),
        ".session.toml.aterm-restore.lock".to_string(),
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
    let mut child = aterm_uds::spawnfd::command("true").spawn().unwrap();
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
    let _scratch = ScratchEnv::runtime("retire");
    let dir = crate::control_auth::socket_dir().expect("scratch control dir");
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
}

/// `write_outgoing` never writes a sidecar through what is already at its
/// path — a planted symlink included — and never fails the handoff for it.
#[test]
fn a_sidecar_is_never_written_through_a_planted_path() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
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
    let _restore = restore_env();
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
    let _restore = restore_env();
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
        // A hang detector, not a latency budget: a blocked open never returns.
        let got = rx.recv_timeout(std::time::Duration::from_secs(60));
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
        panic!("take_incoming did not return within 60 s (blocked opening the FIFO)");
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
            handoff_carry::capture_head(0, &guard, term, turns, None, differ),
        )
    };
    let bytes = handoff_carry::export(&[source], crate::control::turn_ids_for_handoff())
        .remove(0)
        .1;
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

/// Round seven, finding 58: `history` on a ledger a handoff could not
/// carry whole says its floor, so `OK 0` there no longer reads as "no turn
/// landed" — and says nothing for an anchor at or past the floor, or for a
/// ledger carried whole.
#[test]
fn history_after_a_dropped_ledger_says_its_floor() {
    let minted = crate::control::turn_ids_minted() + 50;
    crate::control::raise_turn_ids(minted);
    let h = host(live_engine(1), handoff_carry::adopted_ledger(None));
    let floor = h.ctx.turns.lock().unwrap().unheld_below();
    assert!(floor > minted);
    let reply = crate::control::cmd_history(&h.ctx, "since=40");
    assert_eq!(reply, format!("OK 0 unheld_below={floor}\n"));
    assert_eq!(
        crate::control::cmd_history(&h.ctx, ""),
        format!("OK 0 unheld_below={floor}\n")
    );
    let past = format!("since={}", floor - 1);
    assert_eq!(crate::control::cmd_history(&h.ctx, &past), "OK 0\n");
    let whole = handoff_carry::adopted_ledger(Some(&mut ControlCarry::default()));
    let h = host(live_engine(1), whole);
    assert_eq!(crate::control::cmd_history(&h.ctx, ""), "OK 0\n");
}

/// A turn record for the `history` tests: `id`, nothing else of note.
fn history_rec(id: u64, carried: bool) -> TurnRecord {
    TurnRecord {
        id,
        started_ms: 1,
        dur_ms: 1,
        submitted: true,
        status: "settled",
        text: "t".to_string(),
        screen_hash: 1,
        seq: 1,
        arch: ArchMark::default(),
        by: TurnBy::Ctl,
        carried,
    }
}

/// Round seven, finding 58's review: the floor is said only until the
/// turns it lost would have aged out of the ledger anyway. A supervisor's
/// worker that runs `LEDGER_CAP` more turns after the update has pushed
/// them past the ring, and `history` — and so every `drive ledger` report —
/// goes back to a plain `OK <n>`. Until then it is said, from the first new
/// turn on: that turn's id IS the floor (every id minted after a dropped
/// ledger is at or past it), and a floor at the oldest held record went
/// quiet there, the tab's lost turns unmentioned from one turn after the
/// update on (round seven, findings 204 and 205).
#[test]
fn history_stops_saying_a_floor_once_the_lost_turns_would_have_aged_out() {
    let minted = crate::control::turn_ids_minted() + 50;
    crate::control::raise_turn_ids(minted);
    let h = host(live_engine(1), handoff_carry::adopted_ledger(None));
    let floor = h.ctx.turns.lock().unwrap().unheld_below();
    assert!(floor > minted);
    // The worker's next turn, the first id this process mints.
    h.ctx.turns.lock().unwrap().push(history_rec(floor, false));
    let before = format!("since={}", minted - 10);
    let reply = crate::control::cmd_history(&h.ctx, &before);
    assert!(
        reply.starts_with(&format!("OK 1 unheld_below={floor}\nturn {floor} ")),
        "{reply}"
    );
    assert_eq!(
        h.ctx
            .turns
            .lock()
            .unwrap()
            .carry_floor(crate::control::turn_ids_for_handoff()),
        floor,
        "carried on by a second update"
    );
    // One turn short of LEDGER_CAP: a ledger carried whole still held them.
    let cap = crate::turn_ledger::LEDGER_CAP as u64;
    for id in floor + 1..floor + cap - 1 {
        h.ctx.turns.lock().unwrap().push(history_rec(id, false));
    }
    let reply = crate::control::cmd_history(&h.ctx, "");
    let short = format!("OK {} unheld_below={floor}\n", cap - 1);
    assert!(reply.starts_with(&short), "{}", &reply[..60]);
    h.ctx
        .turns
        .lock()
        .unwrap()
        .push(history_rec(floor + cap - 1, false));
    let reply = crate::control::cmd_history(&h.ctx, &before);
    assert!(
        reply.starts_with(&format!("OK {cap}\n")),
        "{}",
        &reply[..60]
    );
    assert_eq!(
        h.ctx
            .turns
            .lock()
            .unwrap()
            .carry_floor(crate::control::turn_ids_for_handoff()),
        0,
        "not carried on"
    );
}

/// A ledger an update HALVED to fit says its floor too: every record it
/// kept is past the floor, and the records under it were shed (round seven,
/// finding 205's rule: a floor at the oldest held record is not stale).
#[test]
fn history_on_a_halved_ledger_says_its_floor() {
    let minted = crate::control::turn_ids_minted() + 50;
    crate::control::raise_turn_ids(minted);
    let mut halved = ControlCarry {
        turns: vec![history_rec(minted, true)],
        unheld_below: minted,
        shed_below: 0,
        archive: None,
        log: None,
        log_mirror: None,
    };
    let h = host(
        live_engine(1),
        handoff_carry::adopted_ledger(Some(&mut halved)),
    );
    let reply = crate::control::cmd_history(&h.ctx, &format!("since={}", minted - 10));
    assert!(
        reply.starts_with(&format!("OK 1 unheld_below={minted}\nturn {minted} ")),
        "{reply}"
    );
    // An anchor at the floor lost nothing.
    let past = format!("since={}", minted - 1);
    assert!(
        crate::control::cmd_history(&h.ctx, &past).starts_with("OK 1\n"),
        "an anchor at the floor"
    );
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
    // `take_incoming`: past the manifest's `next_turn_id` by one overlap
    // reserve.
    crate::control::raise_turn_ids(crate::seamless::intake_turn_floor(minted));
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
            h.ctx.fabric.clone(),
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
            "",
            &mut sink,
            || true,
        );
        String::from_utf8_lossy(&sink).into_owned()
    };
    // The control: a carried ledger halved to its newest record.
    let mut halved = ControlCarry {
        turns: vec![history_rec(minted, true)],
        unheld_below: minted,
        shed_below: 0,
        archive: None,
        log: None,
        log_mirror: None,
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
        by: TurnBy::Ctl,
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

// ------------------------------------------------ the HISTORY carry, real wire

/// A shell tab with a deep history — far past the screen carry's 256 lines.
fn deep_shell(lines: usize) -> Arc<Mutex<Terminal>> {
    let mut t = Terminal::new(ROWS, COLS);
    for i in 0..lines {
        t.process(format!("build step {i:05} ok\r\n").as_bytes());
    }
    Arc::new(Mutex::new(t))
}

fn hist_files(staged: &StagedHandoff) -> Vec<std::path::PathBuf> {
    let dir = staged.manifest_path.parent().unwrap().to_path_buf();
    std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".hist"))
        .collect()
}

/// The park, for session 0: its screen carry and its history head.
fn park_history(term: &Arc<Mutex<Terminal>>) -> (StageCarry, crate::handoff_history::HistoryHead) {
    let guard = term.lock().unwrap();
    let checkpoint = guard.checkpoint_carry(256).expect("parser is Ground");
    let head = crate::handoff_history::capture_head(0, &guard);
    (
        StageCarry {
            screens: vec![checkpoint],
            controls: Vec::new(),
            next_turn_id: None,
            fg_holders: Vec::new(),
            render_debts: Vec::new(),
        },
        head,
    )
}

/// The worker's history step, as `prepare_outgoing_artifacts` runs it on the
/// fork lane: export the sessions now, then join and stamp.
fn stamp_with(
    term: &Arc<Mutex<Terminal>>,
    carry: &StageCarry,
    head: crate::handoff_history::HistoryHead,
) -> impl Fn(&mut SessionHandoff, &std::path::Path, &str) {
    let term = Arc::clone(term);
    let screens = vec![(0, carry.screens[0].clone())];
    move |manifest, dir, nonce| {
        let results =
            crate::handoff_history::HistoryPlan::Deferred(vec![(0, Arc::clone(&term))].into())
                .results(dir);
        let verdicts = crate::handoff_history::stamp_manifest(
            manifest,
            &screens,
            &[head],
            results,
            dir,
            nonce,
        );
        assert_eq!(verdicts.len(), 1);
    }
}

/// THE HISTORY CARRY OVER THE REAL WIRE: the worker names the sidecar on the
/// manifest record, `take_incoming` opens (and unlinks) it before the proof,
/// the proof is exactly the one the parent expects — the sidecar is in
/// neither digest — and the import after Commit gives the adopted engine the
/// parent's whole history, line for line.
#[test]
fn the_history_carry_crosses_the_real_wire_outside_the_proof() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let term = deep_shell(4000);
    let parent = shell_history(&term.lock().unwrap());
    assert!(parent.len() > 3900);
    let (carry, head) = park_history(&term);
    let stamp = stamp_with(&term, &carry, head);
    let staged = stage_history_handoff("history-wire", &carry, &stamp);
    let published = std::fs::read_to_string(&staged.manifest_path).unwrap();
    assert!(published.contains("history = \""), "{published}");
    assert_eq!(hist_files(&staged).len(), 1, "one attempt-bound sidecar");
    let ((proof, _ready, _), mut adopted) = as_successor(&staged, |_| {
        child_proof_from(take_incoming()).expect("the successor adopts")
    });
    assert_eq!(
        proof, staged.expected,
        "the proof is the one the parent expects"
    );
    assert!(
        hist_files(&staged).is_empty(),
        "the successor unlinks the sidecar as it opens it, before its proof"
    );
    let history = std::mem::take(&mut adopted[0].history);
    assert!(history.carry.is_some(), "the carry rides the adoption");
    assert_eq!((history.dropped, history.lost), (0, 0));
    // The adopt, then the import after Commit.
    let mut history = history;
    let successor = Arc::new(Mutex::new(Terminal::new(ROWS, COLS)));
    crate::spawn::hydrate_adopted_engine(
        &successor,
        Some(adopted[0].checkpoint.as_ref().expect("the screen came")),
        None,
        None,
        None,
        0,
        &mut history,
    );
    let report = crate::handoff_history::run_imports(vec![crate::handoff_history::ImportJob {
        session: 0,
        term: Arc::clone(&successor),
        history,
    }])
    .remove(0);
    assert_eq!(report.failed, None);
    assert_eq!(report.lost(), 0);
    assert_eq!(shell_history(&successor.lock().unwrap()), parent);
    staged.teardown();
}

/// THE ROLLBACK LANE for the history carry: a receiver built before it
/// adopts and proves exactly as before, never opens the `.hist` — and the
/// sender retires the sidecar once the proof checks out.
#[test]
fn a_receiver_built_before_the_history_carry_skips_it_and_proves() {
    let _env = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let _restore = restore_env();
    let term = deep_shell(2000);
    let (carry, head) = park_history(&term);
    let stamp = stamp_with(&term, &carry, head);
    let staged = stage_history_handoff("history-rollback", &carry, &stamp);
    let ((proof, _ready, _), adopted) = as_successor(&staged, |_| {
        child_proof_from(take_incoming_as(ReceiverShape::PreCarry))
            .expect("the older receiver adopts")
    });
    assert_eq!(proof, staged.expected);
    assert!(adopted[0].history.carry.is_none());
    assert_eq!(hist_files(&staged).len(), 1, "left where it was");
    retire_outgoing_controls(&staged.nonce);
    assert!(
        hist_files(&staged).is_empty(),
        "the sender retires it after the proof"
    );
    staged.teardown();
}

/// THE HANDOFF FIXTURE GENERATOR: what this release's producer writes for
/// every required desk and its rendezvous transport. Ordinary tests exercise
/// it in private child processes; the release cut supplies output destinations
/// and its reserved build stamp (`tests/fixtures/handoff/README.md`).
#[cfg(test)]
#[path = "seamless_fixture_generator_tests.rs"]
mod fixture_generator;
