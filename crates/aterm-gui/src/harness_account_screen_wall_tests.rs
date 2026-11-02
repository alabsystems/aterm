// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::session_timeline::{AgentPublication, AgentStamp};

// The native M11 read on 2026-11-02 showed this footer with unknown usage percentages.
// Keep only the vendor's rows; no other session's task text is part of this fixture.
const LAN_SCREEN: &str = "\
  ⎿ You've hit your weekly limit · resets Nov 6 at 12am (America/Los_Angeles)

❯ /rate-limit-options
  ⎿ Claude Code will continue automatically at Nov 6 at 12am. Keep this session open; it may still pause for permission prompts.
     Press esc to cancel the wait.

────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⚠ Usage limit reached · limit resets Nov 6 at 12am
    Continuing automatically at Nov 6 at 12am · esc to cancel · /usage-credits to continue now
  ⏵⏵ bypass permissions on (shift+tab to cycle)";

fn publication(snap: &Snapshot, kind: aterm_phase::WallKind) -> AgentPublication {
    AgentPublication {
        program: Some("claude".into()),
        reader: Some(Program::Claude),
        program_pgid: i32::try_from(snap.pid).unwrap(),
        word: kind.wire(),
        screen_word: kind.wire(),
        screen_word_unix_ms: u64::try_from(ACCT_NOW - 10).unwrap() * 1000,
        ..AgentPublication::default()
    }
}

fn worker(a: &Arc<Accts>, sid: &str, shown: &Arc<Mutex<AgentPublication>>) -> WorkerIdle {
    let mut idle = a.worker(sid, &on());
    let shown_read = Arc::clone(shown);
    idle.hooks.acts.account_screen_wall =
        Arc::new(move |_, snap| screen_account_wall_of(&shown_read.lock().unwrap(), snap));
    let a = Arc::clone(a);
    idle.hooks.acts.account_wall = Arc::new(move |sid, agent, settled| {
        let env = a.procs.lock().unwrap().get(&agent.snap.pid).cloned();
        let read = WallRead {
            env: env.as_deref().unwrap_or(&[]),
            settled: settled(),
            wall: upgrade_drive::named_standing_wall(&a.seat().home, agent.snap),
            screen_wall: agent.screen_wall,
            started: process_started(&agent.snap.start),
        };
        account_wall_of(&a.store(), &a.roster(), sid, &read, a.clock())
    });
    idle
}

#[test]
fn a_screen_only_wall_rotates_and_records_the_current_account() {
    use aterm_phase::WallKind;
    let rows = LAN_SCREEN.lines().map(str::to_owned).collect::<Vec<_>>();
    let generic = aterm_phase::read(Some("claude"), &rows, Some(0))
        .wall
        .unwrap();
    assert_eq!(generic.kind, WallKind::UsageSession);
    let weekly_rows = include_str!(
        "../../aterm-phase/src/fixtures/claude-2.1.299-wall-weekly-auto-continue-armed-34col.txt"
    )
    .lines()
    .skip(1)
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let weekly = aterm_phase::read(Some("claude"), &weekly_rows, Some(0))
        .wall
        .unwrap();
    assert_eq!(weekly.kind, WallKind::UsageWeekly);
    for (kind, window) in [
        (weekly.kind, "seven_day"),
        (generic.kind, accounts::UNNAMED_WINDOW),
    ] {
        for record_first in [false, true] {
            let a = Accts::new();
            let sid = "s-screen-wall";
            let snap = a.spawn(sid, ACCT_TALK_A, "main");
            a.transcript_row(ACCT_TALK_A, r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"You've hit your weekly limit"}]}}"#);
            assert!(upgrade_drive::named_standing_wall(&a.seat().home, &snap).is_none());
            let point = AccountPoint::Wall { kind };
            assert_eq!(
                moves(&a.worker(sid, &on()).account(point)),
                None,
                "control: no measured screen and no quota row cannot cause a rotation"
            );
            let shown = Arc::new(Mutex::new(publication(&snap, kind)));
            let idle = worker(&a, sid, &shown);
            if record_first {
                idle.limited(true);
                assert_eq!(a.state().sessions[sid].account, "main");
                assert!(a.state().account("main").walls.contains_key(window));
            }
            assert_eq!(moves(&idle.account(point)), Some("work-2"));
            assert_eq!(
                idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
                Some("adopted")
            );
            let state = a.state();
            assert_eq!(state.active.as_deref(), Some("work-2"));
            let wall = &state.account("main").walls[window];
            assert_eq!((wall.at, wall.until), (ACCT_NOW - 10, None));
            assert!(state.account("work-2").walls.is_empty());
            assert_eq!(state.rotations.len(), 1);
            assert!(a.ledger().contains(&"wall"));
            assert!(a.ledger().contains(&"rotate"));
            assert_eq!(
                screen_account_wall_of(
                    &shown.lock().unwrap(),
                    &idle.kept.lock().unwrap().snapshot.clone().unwrap()
                ),
                None
            );
            let _ = idle.account(point);
            assert!(a.state().account("work-2").walls.is_empty());
            assert_eq!(
                a.state().rotations.len(),
                1,
                "an old screen cannot rotate the new process"
            );
        }
    }
}

#[test]
fn a_screen_wall_read_requires_the_current_complete_frame_and_process() {
    let a = Accts::new();
    let snap = a.spawn("s-frame", ACCT_TALK_A, "main");
    let store = crate::session_store::new_store();
    let handle = crate::session_store::test_handle(1);
    store.write().unwrap().register(handle.clone());
    let publish = || {
        let term = handle.term.lock().unwrap();
        let mut timeline = handle.ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(i32::try_from(snap.pid).unwrap());
        timeline.set_program(i32::try_from(snap.pid).unwrap(), Some("claude".into()));
        timeline.publish_agent(
            "wall:usage-weekly",
            None,
            None,
            Some(Program::Claude),
            AgentStamp {
                generation: crate::control::screen_gen(&term),
                fp: 1,
            },
        );
    };
    handle
        .term
        .lock()
        .unwrap()
        .process(LAN_SCREEN.replace('\n', "\r\n").as_bytes());
    publish();
    assert!(screen_account_wall(&store, handle.sid.as_str(), &snap).is_some());
    let mut other = snap.clone();
    other.pid += 1;
    assert_eq!(
        screen_account_wall(&store, handle.sid.as_str(), &other),
        None
    );
    handle
        .term
        .lock()
        .unwrap()
        .process(b"\x1b[?2026hnew output");
    assert_eq!(
        screen_account_wall(&store, handle.sid.as_str(), &snap),
        None
    );
    handle.term.lock().unwrap().process(b"\x1b[?2026l");
    assert_eq!(
        screen_account_wall(&store, handle.sid.as_str(), &snap),
        None,
        "publication still names the earlier frame"
    );
    publish();
    assert!(screen_account_wall(&store, handle.sid.as_str(), &snap).is_some());
}

#[test]
fn screen_wall_identity_and_non_usage_errors_cannot_exhaust_an_account() {
    let a = Accts::new();
    let snap = a.spawn("s-screen-control", ACCT_TALK_A, "main");
    let base = publication(&snap, aterm_phase::WallKind::UsageWeekly);
    for word in [
        "idle",
        "busy",
        "wall:auth",
        "wall:model-bucket",
        "wall:spend",
        "wall:context",
        "wall:api-error",
        "wall:overloaded",
        "wall:memory",
    ] {
        let mut published = base.clone();
        published.word = word;
        published.screen_word = word;
        assert_eq!(screen_account_wall_of(&published, &snap), None, "{word}");
    }
    let mut published = base.clone();
    published.reader = Some(Program::Codex);
    assert_eq!(screen_account_wall_of(&published, &snap), None);
    published = base.clone();
    published.program = Some("zsh".into());
    assert_eq!(screen_account_wall_of(&published, &snap), None);
    published = base;
    published.screen_word_unix_ms = u64::try_from(ACCT_NOW - 101).unwrap() * 1000;
    assert_eq!(
        screen_account_wall_of(&published, &snap),
        None,
        "a wall from before this exec"
    );
    let mut unbound = snap.clone();
    unbound.session = None;
    assert_eq!(
        screen_account_wall_of(
            &publication(&snap, aterm_phase::WallKind::UsageWeekly),
            &unbound
        ),
        None
    );
}

#[test]
fn a_screen_wall_keeps_a_clear_and_a_transcripts_process_boundary() {
    let a = Accts::new();
    let sid = "s-screen-clear";
    let snap = a.spawn(sid, ACCT_TALK_A, "main");
    let shown = Arc::new(Mutex::new(publication(
        &snap,
        aterm_phase::WallKind::UsageWeekly,
    )));
    let idle = worker(&a, sid, &shown);
    idle.limited(true);
    assert!(a.state().account("main").walls.contains_key("seven_day"));
    let roster = a.roster();
    a.store()
        .transact(ACCT_NOW, &mut None, |state| {
            Ok(vec![accounts::pin(
                state,
                &roster,
                &accounts::enrolled(&roster),
                "main",
                ACCT_NOW,
            )?])
        })
        .unwrap();
    a.now.store(ACCT_NOW + 30, Ordering::SeqCst);
    idle.limited(true);
    assert!(
        a.state().account("main").walls.is_empty(),
        "the same screen has its original observation time"
    );
    assert_eq!(
        moves(&idle.account(AccountPoint::Wall {
            kind: aterm_phase::WallKind::UsageWeekly
        })),
        None
    );

    let a = Accts::new();
    let snap = a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 500);
    let shown = Arc::new(Mutex::new(publication(
        &snap,
        aterm_phase::WallKind::UsageWeekly,
    )));
    let idle = worker(&a, sid, &shown);
    idle.limited(true);
    assert!(
        a.state().account("main").walls.is_empty(),
        "a screen cannot reattribute an old transcript rejection"
    );
    assert_eq!(
        moves(&idle.account(AccountPoint::Wall {
            kind: aterm_phase::WallKind::UsageWeekly
        })),
        None
    );
}

#[test]
fn a_screen_wall_still_reports_a_move_that_landed_after_its_wait() {
    let a = Accts::new();
    let sid = "s-screen-untold";
    let snap = a.spawn(sid, ACCT_TALK_A, "main");
    let kind = aterm_phase::WallKind::UsageWeekly;
    let point = AccountPoint::Wall { kind };
    let shown = Arc::new(Mutex::new(publication(&snap, kind)));
    let idle = worker(&a, sid, &shown);
    a.unread.store(true, Ordering::SeqCst);
    assert_eq!(moves(&idle.account(point)), Some("work-2"));
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-unverified")
    );
    a.readable();
    // A newly published copy of the resumed wall is still the landing's acknowledgement.
    let next = a.tabs.lock().unwrap()[sid].clone();
    let mut published = publication(&next, kind);
    published.screen_word_unix_ms = u64::try_from(ACCT_NOW).unwrap() * 1000;
    *shown.lock().unwrap() = published;
    idle.limited(true);
    assert!(a.state().account("work-2").walls.is_empty());
    assert_eq!(moves(&idle.account(point)), Some("work-2"));
    assert!(a.state().account("work-2").walls.is_empty());
    assert_eq!(a.state().rotations.len(), 1);
}
