// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spawn_session` seeds a seamless update's carried rows onto the adopted session it returns:
//! the broadcast opt-ins, the standing halt and the fabric counters, the supervisor's claim and
//! the attention lines, the task record, the Autopilot end, the book of declared wakes, the
//! render debt and the fallback title (`SessionRecord`). An adopted shell forks nothing, so these
//! tests run the product function itself, on a real PTY master and a pid no teardown signals.

use std::sync::Arc;

use aterm_agent::harness::acts::TaskFace;
use aterm_core::terminal::{RenderDebt, TerminalCheckpoint};
use aterm_session::{LaunchNonce, SessionId};

use super::{Adopted, HandedMaster, WakePost, WakeTarget, spawn_session};
use crate::fabric::{FabricCarry, Hold};
use crate::harness_end_carry::{CarriedEnd, EndCarry};
use crate::harness_glass::EndFace;
use crate::task_record::TaskState;
use crate::{App, WindowId};

/// A worker's task record as the park writes it (`TaskRecord::carry_row`).
const TASK_ROW: &str =
    "rev=3 state=done from=ctl since=1000 result_bytes=12 attention=0 why=marker";

/// When the carried end's task landed, unix ms.
const LANDED_AT_MS: i64 = 1_792_000_000_000;

struct NoWakes;

impl WakeTarget for NoWakes {
    fn wake_post(&self) -> WakePost {
        Arc::new(|_| true)
    }
}

/// The rows one session's handoff record carries.
#[derive(Default)]
struct Rows {
    topics: Vec<String>,
    holds: Vec<String>,
    fabric: FabricCarry,
    supervisor: Option<String>,
    attention: Vec<String>,
    task: Option<String>,
    end: Option<String>,
    wakes: Option<String>,
    screen: Option<TerminalCheckpoint>,
    render_debt: Option<RenderDebt>,
    title: String,
}

/// What the adopted session came up holding: its broadcast opt-ins (topic, since), standing
/// halts and newest inbox row id; its supervisor's holder and attention line; its task record
/// (rev, state, why, result bytes), the time its timeline holds a landed face from, and its
/// book's `status` fields; its engine's render debt and window title.
#[derive(Debug, PartialEq, Eq)]
struct Seeded {
    topics: Vec<(String, String)>,
    holds: Vec<Hold>,
    last_msg_id: u64,
    supervisor: Option<String>,
    attention: Option<String>,
    task: Option<(u64, TaskState, Option<&'static str>, u64)>,
    landed_at: Option<i64>,
    waits: String,
    render_debt: RenderDebt,
    title: String,
}

/// A session that came up holding no carried row.
fn nothing() -> Seeded {
    Seeded {
        topics: Vec::new(),
        holds: Vec::new(),
        last_msg_id: 0,
        supervisor: None,
        attention: None,
        task: None,
        landed_at: None,
        waits: "waits=0 waits_ready=0".to_string(),
        render_debt: RenderDebt::default(),
        title: String::new(),
    }
}

/// The carried end of a task that landed unseen, as the park stamps it (`EndCarry::row`).
fn end_row() -> String {
    EndCarry {
        end: Some(CarriedEnd {
            face: EndFace::Landed,
            at_ms: LANDED_AT_MS,
            seen: false,
        }),
        face: None,
        worked_ms: 0,
    }
    .row()
    .expect("an end has a row")
}

/// A book of one declared wake for this process's own agent, as the park carries it
/// (`occupancy_wakes::carry_row`). macOS only: the agent's identity is this process's kernel
/// record, and `kproc` reads one only there.
#[cfg(target_os = "macos")]
fn book_row() -> Option<String> {
    use crate::occupancy_wakes::ProcTree as _;
    use aterm_agent::supervise::occupancy::declarer::AgentId;
    let me = crate::occupancy_wakes::Kernel
        .link(std::process::id())
        .expect("this process's own record");
    let parent = SessionId::generate();
    crate::occupancy_wakes::book_for_test(
        parent.as_str(),
        0,
        AgentId {
            pid: me.pid,
            birth_us: me.birth_us,
        },
        "label=t when=in:1h",
        crate::harness_screen::unix_ms_now(),
    );
    let row = crate::occupancy_wakes::carry_row(parent.as_str());
    crate::occupancy_wakes::forget(parent.as_str());
    Some(row.expect("a book to carry"))
}

#[cfg(not(target_os = "macos"))]
fn book_row() -> Option<String> {
    None
}

/// A carried screen on the alternate screen, the only one a render debt is owed on
/// (`Terminal::seed_render_debt`).
fn alternate_screen() -> TerminalCheckpoint {
    let mut engine = aterm_core::terminal::Terminal::new(24, 80);
    engine.process(b"\x1b[?1049h");
    engine.checkpoint()
}

/// A shell handed across the exec on `master`, carrying `rows`. Its pid is -1: teardown
/// signals no process for it (`aterm_pty::hangup_shell`).
fn handed(master: i32, rows: Rows) -> Adopted {
    Adopted {
        repaint: false,
        local_id: 3,
        master: HandedMaster::new(master),
        pid: -1,
        sid: SessionId::generate(),
        nonce: LaunchNonce::generate(),
        checkpoint: rows.screen,
        control: None,
        frozen_path: false,
        topics: rows.topics,
        fg_holder: 0,
        rekey: false,
        loader: false,
        history: crate::handoff_history::AdoptedHistory::default(),
        holds: rows.holds,
        title: rows.title,
        supervisor: rows.supervisor,
        attention_owners: rows.attention,
        claim_grace: false,
        outgoing_build: None,
        keeper: None,
        render_debt: rows.render_debt,
        fabric: rows.fabric,
        timeline_id: 0,
        log: Default::default(),
        journal_key: None,
        task: rows.task,
        autopilot_end: rows.end,
        wakes: rows.wakes,
        box_receipt: None,
    }
}

/// Adopt a handed shell carrying `rows` through `spawn_session`, under a seamless successor's
/// factory (its readers wait for the Commit), and read what the session came up holding.
fn adopt(rows: Rows) -> Seeded {
    let claims = aterm_tempfile::tempdir().expect("a scratch control dir");
    let _rendezvous = crate::identity_claim::rendezvous_override(claims.path());
    let app = App::headless_for_test();
    let mut factory = app.session_factory.clone();
    factory.defer_adopted_readers = true;
    // A carried screen's restore asks who holds the terminal; no group does here.
    factory.fg_probe = |_| -1;
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty pair");
    let session = spawn_session(
        7,
        WindowId(0),
        24,
        80,
        None,
        &factory,
        &NoWakes,
        None,
        None,
        None,
        Some(handed(master, rows)),
    )
    .expect("the handed shell is adopted");
    let sid = session.ctx.self_id.as_str().to_string();
    let fabric = &session.ctx.fabric;
    let topics = fabric
        .topics()
        .into_iter()
        .map(|(topic, entry)| (topic, entry.since))
        .collect();
    let holds = fabric.holds().standing().collect();
    let last_msg_id = fabric.last_msg_id();
    let (supervisor, attention) = {
        let meta = session.ctx.meta.lock().unwrap();
        (
            meta.supervisor.as_ref().map(|claim| claim.holder.clone()),
            meta.attention.clone(),
        )
    };
    let (task, landed_at) = {
        let timeline = session.ctx.timeline.lock().unwrap();
        (
            timeline
                .task()
                .map(|t| (t.rev, t.state, t.why, t.result_bytes)),
            timeline.task_face_at(TaskFace::Landed),
        )
    };
    let waits = crate::occupancy_wakes::status_fields(Some(&sid));
    crate::occupancy_wakes::forget(&sid);
    let (render_debt, title) = {
        let engine = crate::term_lock(&session.term);
        (engine.render_debt(), engine.title().to_string())
    };
    drop(session);
    aterm_pty::close_fd(slave);
    crate::identity_claim::release_claims_for_test();
    Seeded {
        topics,
        holds,
        last_msg_id,
        supervisor,
        attention,
        task,
        landed_at,
        waits,
        render_debt,
        title,
    }
}

/// EACH CARRIED ROW COMES UP ON THE ADOPTED SESSION, and only its own: the opt-ins, halt and
/// counters on its fabric, the claim and the attention line on its meta, the task record on its
/// timeline, the landed end held for the loop to restate, the book open where its agent lives,
/// the debt on its restored alternate screen and the title on its engine.
/// NEGATIVE CONTROLS: an alternate screen carried with no debt comes up owing none, and a shell
/// that carried no row comes up with none of them.
#[test]
fn an_adopted_shell_comes_up_holding_each_row_it_carried() {
    let mut arms = vec![
        (
            "the broadcast opt-ins",
            Rows {
                topics: vec!["news head".to_string()],
                ..Rows::default()
            },
            Seeded {
                topics: vec![("news".to_string(), "head".to_string())],
                ..nothing()
            },
        ),
        (
            "the standing halt",
            Rows {
                holds: vec!["local mine".to_string()],
                ..Rows::default()
            },
            Seeded {
                holds: vec![Hold {
                    reason: "mine".to_string(),
                    origin: "local".to_string(),
                }],
                ..nothing()
            },
        ),
        (
            "the fabric counters",
            Rows {
                fabric: FabricCarry {
                    last_msg_id: 5,
                    ..FabricCarry::default()
                },
                ..Rows::default()
            },
            Seeded {
                last_msg_id: 5,
                ..nothing()
            },
        ),
        (
            "the supervisor's claim",
            Rows {
                supervisor: Some("watch-bob 60000".to_string()),
                ..Rows::default()
            },
            Seeded {
                supervisor: Some("watch-bob".to_string()),
                ..nothing()
            },
        ),
        (
            "the attention line",
            Rows {
                attention: vec!["ci@host red%20build".to_string()],
                ..Rows::default()
            },
            Seeded {
                attention: Some("red build".to_string()),
                ..nothing()
            },
        ),
        (
            "the task record",
            Rows {
                task: Some(TASK_ROW.to_string()),
                ..Rows::default()
            },
            Seeded {
                task: Some((3, TaskState::Done, Some("marker"), 12)),
                ..nothing()
            },
        ),
        (
            "the Autopilot end",
            Rows {
                end: Some(end_row()),
                ..Rows::default()
            },
            Seeded {
                landed_at: Some(LANDED_AT_MS),
                ..nothing()
            },
        ),
        (
            "the render debt",
            Rows {
                screen: Some(alternate_screen()),
                render_debt: Some(RenderDebt::of_rows(-1, 0)),
                ..Rows::default()
            },
            Seeded {
                render_debt: RenderDebt::of_rows(-1, 0),
                ..nothing()
            },
        ),
        (
            "NEGATIVE CONTROL: an alternate screen carried with no debt owes none",
            Rows {
                screen: Some(alternate_screen()),
                ..Rows::default()
            },
            nothing(),
        ),
        (
            "the fallback title",
            Rows {
                title: "vim main.rs".to_string(),
                ..Rows::default()
            },
            Seeded {
                title: "vim main.rs".to_string(),
                ..nothing()
            },
        ),
        (
            "NEGATIVE CONTROL: nothing carried, nothing seeded",
            Rows::default(),
            nothing(),
        ),
    ];
    if let Some(book) = book_row() {
        arms.push((
            "the book of declared wakes",
            Rows {
                wakes: Some(book),
                ..Rows::default()
            },
            Seeded {
                waits: "waits=1 waits_ready=0".to_string(),
                ..nothing()
            },
        ));
    }
    for (what, rows, want) in arms {
        assert_eq!(adopt(rows), want, "{what}");
    }
}
