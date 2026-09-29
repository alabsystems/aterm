// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE GOAL PAUSE THROUGH THE LANE'S OWN DRIVER (the owner's decision of
//! 2026-09-28): the stand-in instance and kernel of the Codex lane's tests,
//! with a goal on the tab's Codex ([`GoalWorld`]) — its footer moved by the
//! `/goal pause` and `/goal resume` the lane types, the relaunched Codex
//! opening on its `Resume paused goal?` box — and the tab's goal record
//! (`goal_hold`) under the aterm state root. Every visit here is a fresh
//! call over what the files hold, as a host that restarted between two of
//! them would make it: what the lane owes is read from the record, never
//! remembered.

use super::*;
use crate::harness::goal_hold::{self, Hold, How, Owner, Stage};

/// A rig whose tab's Codex pursues a goal (`takes`: Codex takes a typed
/// pause), its embedded conversation's rollout running a goal turn, the
/// upgrade three hours behind (the Land rung), and the aterm state root the
/// goal record and the switch's ledger live under.
fn goal_rig(name: &str, takes: bool) -> (Rig, Arc<Mutex<GoalWorld>>) {
    let goal = Arc::new(Mutex::new(GoalWorld {
        takes,
        ..GoalWorld::default()
    }));
    let world = Arc::clone(&goal);
    let mut rig = Rig::new(name, T2, true, move |w| w.goal = Some(world));
    rig.opts.aterm_state = Some(rig.dir.join("aterm"));
    rollout_push(
        &rig,
        &[
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"2"}}"#,
            r#"{"type":"turn_context","payload":{"cwd":"/w"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<codex_internal_context source=\"goal\">Continue working toward the active thread goal</codex_internal_context>"}]}}"#,
        ],
    );
    let now = now_s();
    save(
        &rig.opts,
        &key(TAB),
        &St {
            agent: Agent::Codex,
            from: "0.157.0".into(),
            to: "0.157.1".into(),
            source: "managed".into(),
            salt: now - 3 * 3_600,
            pending_since: now - 3 * 3_600,
            last_seq: 11,
            seq_since_s: now - 60,
            notice_pid: TUI,
            notice_start: String::new(),
            tab: TAB.into(),
            ..St::default()
        },
    );
    (rig, goal)
}

/// Lines appended to the rig's rollout, and the file aged a minute.
fn rollout_push(rig: &Rig, lines: &[&str]) {
    let mut text = std::fs::read_to_string(&rig.rollout).expect("rollout");
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    std::fs::write(&rig.rollout, text).expect("rollout");
    age(&rig.rollout);
}

/// The goal's turn ends (its `task_complete`): a paused goal starts none after it.
fn turn_ends(rig: &Rig) {
    rollout_push(
        rig,
        &[r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"2"}}"#],
    );
}

/// The tab's goal record as the lane left it.
fn hold(rig: &Rig) -> Option<Hold> {
    let root = rig.opts.aterm_state.as_deref().expect("a root");
    goal_hold::read(&goal_hold::path(root, TAB))
}

fn write_hold(rig: &Rig, h: &Hold) {
    let root = rig.opts.aterm_state.as_deref().expect("a root");
    goal_hold::write(&goal_hold::path(root, TAB), h).expect("write");
}

/// What the lane TYPED as a line, each by its text's last word.
fn lines(rig: &Rig) -> Vec<String> {
    rig.typed()
        .iter()
        .map(|l| {
            if l.ends_with(" /goal pause") {
                "/goal pause".to_string()
            } else if l.ends_with(" /goal resume") {
                "/goal resume".to_string()
            } else if l.ends_with(" /exit") {
                "/exit".to_string()
            } else if l.contains(cx::ANNOUNCE_HEAD) {
                "notice".to_string()
            } else if l.contains("'resume'") {
                "relaunch".to_string()
            } else {
                l.clone()
            }
        })
        .collect()
}

/// The keys the lane pressed (`key …` requests).
fn keys_pressed(rig: &Rig) -> Vec<String> {
    rig.asked()
        .into_iter()
        .filter(|l| l.contains(" key "))
        .collect()
}

/// THE WHOLE MOVE, A GOAL PAUSED FOR IT: an embedded Codex whose goal's
/// turn runs (its rollout's last turn event `task_started`), three hours
/// behind — the Land rung. The first look PAUSES the goal (`/goal pause`,
/// its record claimed first, the footer showing it paused); while the goal's
/// last turn runs the look HOLDS (`wait:goal-held`, nothing typed); once it
/// ends, the ordinary step asks for the stopping point (the notice) — no
/// goal turn follows; at the READY answer the `/exit` and the relaunch, and
/// the relaunched Codex, opening on its paused goal's box, has it answered
/// with ONE guarded Enter on its focused `Resume goal` — the goal resumed,
/// which is the carry-on: no continuation typed over it (`done:goal`).
/// Each visit is a fresh call over the files: a host restarted between any
/// two resumes from the record.
#[test]
fn an_embedded_goal_is_paused_at_land_moved_and_resumed_once() {
    let (rig, goal) = goal_rig("goal-move", true);
    let r = rig.visit(None);
    assert_eq!(r.step, "goal-paused", "{r:?}\n{:#?}", rig.asked());
    assert_eq!(lines(&rig), ["/goal pause"]);
    let typed = rig.typed();
    assert!(
        typed[0].contains("submit=guarded:^[›»]\\s.*pause") || typed[0].contains("pause\\s*$"),
        "guarded on the composer's row: {}",
        typed[0]
    );
    let h = hold(&rig).expect("claimed");
    assert_eq!(
        (h.owner, h.stage, h.how, h.pid),
        (Owner::Upgrade, Stage::Paused, How::Typed, TUI)
    );
    // The goal's last turn runs on: held, nothing typed, nothing pressed.
    let r = rig.visit(None);
    assert_eq!(r.step, "wait:goal-held", "{r:?}");
    assert_eq!(lines(&rig), ["/goal pause"]);
    assert!(keys_pressed(&rig).is_empty());
    // It ends; no goal turn follows a paused goal's last: the notice.
    turn_ends(&rig);
    let r = rig.visit(None);
    assert_eq!(r.step, "announced:1", "{r:?}");
    let marker = rig.state().marker;
    rollout_push(
        &rig,
        &[&format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Stopped.\n{marker}"}}]}}}}"#
        )],
    );
    let r = rig.visit(None);
    assert_eq!(r.step, "done:goal", "{r:?}\n{:#?}", rig.asked());
    assert_eq!(
        lines(&rig),
        ["/goal pause", "notice", "/exit", "relaunch"],
        "no continuation typed over the resumed goal"
    );
    let keys = keys_pressed(&rig);
    assert_eq!(keys.len(), 1, "{keys:#?}");
    assert!(
        keys[0].contains("if-gen=1.40 ")
            && keys[0].contains("Resume\\x20goal")
            && keys[0].ends_with(" enter"),
        "fenced, guarded on the focused row: {}",
        keys[0]
    );
    let g = goal.lock().expect("goal");
    assert_eq!((g.paused, g.boxed), (false, false), "pursued again");
    assert_eq!(g.seen, ["pause", "enter"], "one pause, one resume");
    drop(g);
    let h = hold(&rig).expect("kept");
    assert_eq!(
        (h.stage, h.why.as_str(), h.resumes),
        (Stage::Resumed, "moved", 1)
    );
    let steps = rig.ledger_steps();
    for want in [
        "goal-paused",
        "announced:1",
        "exit-typed",
        "relaunched",
        "goal-resumed:moved",
        "done:goal",
    ] {
        assert!(steps.iter().any(|s| s == want), "{want} in {steps:?}");
    }
}

/// A PERSON'S HAND IN BETWEEN: the goal aterm paused is resumed by the
/// person before the move — the next look finds it pursued with no resume of
/// aterm's, and RELEASES it (the goal is theirs: aterm never resumes it, and
/// pauses it again only after the rest); the goal holds the tab again as a
/// running turn does. NEGATIVE CONTROL: no `/goal resume` is ever typed.
#[test]
fn a_person_resuming_the_paused_goal_takes_it_from_aterm() {
    let (rig, goal) = goal_rig("goal-person", true);
    assert_eq!(rig.visit(None).step, "goal-paused");
    {
        let mut g = goal.lock().expect("goal");
        g.paused = false;
        g.human_ms = Some(5_000);
    }
    let r = rig.visit(None);
    assert_eq!(r.step, "goal-released:by-hand", "{r:?}");
    let h = hold(&rig).expect("kept");
    assert_eq!((h.stage, h.why.as_str()), (Stage::Released, "by-hand"));
    assert!(!cx::goal_owed(Some(&h)));
    // Their goal runs on: the look waits on it, and pauses nothing (the rest).
    goal.lock().expect("goal").human_ms = None;
    let r = rig.visit(None);
    assert_eq!(r.step, "wait:goal", "{r:?}");
    assert_eq!(lines(&rig), ["/goal pause"], "nothing more typed");
    assert!(keys_pressed(&rig).is_empty());
}

/// A HOST RESTARTED BETWEEN THE RESUME AND ITS SHOWING: the record says
/// `Resuming` (written before the key). The Codex current in the tab (the
/// move made): where its footer shows the goal pursued, the next look marks
/// it resumed and types nothing; where it still shows it paused, it waits
/// out the resume's take window, then types `/goal resume` ONCE more — never
/// twice at one look. NEGATIVE CONTROL: a record with nothing owed
/// (resumed) over a paused footer types nothing.
#[test]
fn a_resume_made_before_a_restart_is_never_made_twice() {
    let (mut rig, goal) = goal_rig("goal-restart", true);
    // The move made: a current Codex in the tab, the relaunch aterm made.
    rig.tui.version = v("0.157.1");
    rig.tui.pid = NEW;
    save(
        &rig.opts,
        &key(TAB),
        &St {
            phase: Phase::Done,
            relaunched_pid: NEW,
            resumed_pid: NEW,
            ..rig.state()
        },
    );
    let now = now_s();
    let resuming = Hold {
        stage: Stage::Resuming,
        took_at: now - 600,
        resume_at: now - 5,
        resumes: 1,
        why: "moved".into(),
        ..Hold::pausing(Owner::Upgrade, How::Typed, TUI, "0.157.1", now - 900)
    };
    write_hold(&rig, &resuming);
    // Pursued: resumed; nothing typed.
    let r = rig.visit(None);
    assert_eq!(r.step, "goal-resumed", "{r:?}");
    assert_eq!(hold(&rig).map(|h| h.stage), Some(Stage::Resumed));
    assert!(lines(&rig).is_empty());
    // Still paused within the take window: a wait.
    write_hold(&rig, &resuming);
    goal.lock().expect("goal").paused = true;
    assert_eq!(rig.visit(None).step, "wait:goal-resuming");
    assert!(lines(&rig).is_empty());
    // Past it: once more.
    write_hold(
        &rig,
        &Hold {
            resume_at: now - cx::RESUME_TAKE_S - 1,
            ..resuming.clone()
        },
    );
    let r = rig.visit(None);
    assert_eq!(r.step, "goal-resumed:moved", "{r:?}");
    assert_eq!(lines(&rig), ["/goal resume"]);
    assert_eq!(
        hold(&rig).map(|h| (h.stage, h.resumes)),
        Some((Stage::Resumed, 2))
    );
    // Owed nothing: a paused footer is the person's.
    goal.lock().expect("goal").paused = true;
    assert_eq!(rig.visit(None).step, "current");
    assert_eq!(lines(&rig), ["/goal resume"]);
}

/// AN OPEN SAVE-THEN-WAIT SWITCH KEEPS THE PAUSE OFF: the tab's loop ledger
/// holds an open wind-down — the session is the switch's — so the look at
/// Land pauses nothing and waits on the goal as before. And a hold the
/// SWITCH has on the goal record refuses the upgrade's claim. NEGATIVE
/// CONTROL: the switch's wind-down closed (`phase=done`), the pause is made.
#[test]
fn an_open_save_then_wait_switch_keeps_the_pause_off() {
    let (rig, _goal) = goal_rig("goal-switch", true);
    let root = rig.opts.aterm_state.clone().expect("a root");
    let ledger = crate::supervise::approvals::ledger_under(&root, Some(TAB));
    std::fs::create_dir_all(ledger.parent().expect("dir")).expect("drive");
    let row = |phase: &str| {
        format!(
            r#"{{"sid":"{TAB}","ts":1,"rule_id":"model-wind-down@v1","outcome":"skipped","reason":"the turn-end policy (model switch: kind=wind-down from=GPT-6-Astra effort=ultra to=gpt-6-luna back_at=- marker=ATERM-SAVED-1 goal=- pause=- stops=0 since=- esc=- opened=- pressed=- told=- phase={phase})"}}"#
        ) + "\n"
    };
    std::fs::write(&ledger, row("owed")).expect("ledger");
    let r = rig.visit(None);
    assert_eq!(r.step, "wait:goal", "{r:?}");
    assert!(lines(&rig).is_empty(), "nothing typed under the switch");
    // The switch holding the goal on its record: the claim is refused.
    std::fs::write(&ledger, row("done")).expect("ledger");
    let held = goal_hold::sync_switch(&goal_hold::path(&root, TAB), true, How::Esc, now_s());
    assert_eq!(held.map(|h| h.owner), Some(Owner::Switch));
    assert_eq!(rig.visit(None).step, "wait:goal");
    assert!(lines(&rig).is_empty());
    // The switch gone: the pause.
    let _ = goal_hold::sync_switch(&goal_hold::path(&root, TAB), false, How::Esc, now_s());
    assert_eq!(rig.visit(None).step, "goal-paused");
    assert_eq!(lines(&rig), ["/goal pause"]);
}

/// THE ESC AT A TURN'S HEAD, the fallback for a typed pause that never shows
/// (a build that would not take `/goal pause` mid-turn): past the take
/// window, with the goal thread's rollout at a turn's HEAD — `task_started`,
/// its context, the goal's message and the `world_state` 0.158.0 writes
/// there (measured read-only in the owner's rollouts, 2026-09-28: between a
/// turn's opening messages and its `turn_context`), nothing of its own work
/// — and the screen as a head draws it, the turn's STATUS ROW up (`•
/// Working (0s • esc to interrupt)`: the goal-pause review of 2026-09-28
/// found the Esc weighed on a composer "free" of that row, which a head
/// always shows, so it could never be pressed) — ONE Esc, fenced and
/// guarded on the footer's goal row, the record naming it first; the goal
/// shows paused. NEGATIVE CONTROL: the turn past its head (a tool call in
/// its rollout) — no Esc, whatever the wait.
#[test]
fn the_esc_is_pressed_only_at_a_goal_turns_head() {
    let (rig, goal) = goal_rig("goal-esc", false);
    assert_eq!(rig.visit(None).step, "goal-paused");
    assert_eq!(
        hold(&rig).map(|h| h.stage),
        Some(Stage::Pausing),
        "not shown"
    );
    assert_eq!(rig.visit(None).step, "wait:goal-pausing", "its take window");
    let aged = |rig: &Rig| {
        let h = hold(rig).expect("held");
        write_hold(
            rig,
            &Hold {
                tried_at: h.tried_at - cx::PAUSE_TAKE_S - 1,
                at: h.at - cx::PAUSE_TAKE_S - 1,
                ..h
            },
        );
    };
    // Past its head: no Esc.
    rollout_push(
        &rig,
        &[
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec","arguments":"{}"}}"#,
        ],
    );
    aged(&rig);
    goal.lock().expect("goal").working = true;
    assert_eq!(rig.visit(None).step, "wait:goal-pausing");
    assert!(keys_pressed(&rig).is_empty(), "never past a head");
    // The next goal turn, at its head, its status row up.
    rollout_push(
        &rig,
        &[
            r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"2"}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"3"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<codex_internal_context source=\"goal\">Continue</codex_internal_context>"}]}}"#,
            r#"{"timestamp":"2026-09-28T15:28:29.649Z","ordinal":6,"type":"world_state","payload":{"full":true,"state":{}}}"#,
            r#"{"type":"turn_context","payload":{"cwd":"/w"}}"#,
        ],
    );
    aged(&rig);
    let r = rig.visit(None);
    assert_eq!(r.step, "goal-esc", "{r:?}\n{:#?}", rig.asked());
    let keys = keys_pressed(&rig);
    assert_eq!(keys.len(), 1, "{keys:#?}");
    assert!(
        keys[0].contains("if-gen=1.11 ")
            && keys[0].contains("Pursuing\\x20goal")
            && keys[0].ends_with(" esc"),
        "fenced, guarded on the footer's goal row: {}",
        keys[0]
    );
    let h = hold(&rig).expect("held");
    assert_eq!((h.stage, h.how), (Stage::Paused, How::Esc));
    assert_eq!(goal.lock().expect("goal").seen, ["pause", "esc"]);
}

/// THE RELAUNCH'S BOX IS ANSWERED THOUGH ITS STARTUP FRAME COMES FIRST (the
/// goal-pause review of 2026-09-28, measured live on 0.158.0: `codex resume
/// <thread>` over a paused goal draws its startup composer, footerless,
/// about 200 ms BEFORE the `Resume paused goal?` box): the whole move, the
/// relaunched Codex showing that frame at its first reads. The upgrade waits
/// for the box — or a footer that names the goal — never the composer
/// alone, and answers the box itself with ONE guarded Enter: the goal
/// resumed, the carry-on its own. Until that day the wait ended on the
/// startup frame, decided a wait there (no footer), and no later look of the
/// upgrade's ever pressed the box.
#[test]
fn the_relaunchs_box_is_answered_though_its_startup_frame_comes_first() {
    let (rig, goal) = goal_rig("goal-startup", true);
    assert_eq!(rig.visit(None).step, "goal-paused");
    turn_ends(&rig);
    assert_eq!(rig.visit(None).step, "announced:1");
    let marker = rig.state().marker;
    rollout_push(
        &rig,
        &[&format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Stopped.\n{marker}"}}]}}}}"#
        )],
    );
    goal.lock().expect("goal").startup = 3;
    let r = rig.visit(None);
    assert_eq!(r.step, "done:goal", "{r:?}\n{:#?}", rig.asked());
    let g = goal.lock().expect("goal");
    assert_eq!(g.startup, 0, "the startup frames were read");
    assert_eq!((g.paused, g.boxed), (false, false), "pursued again");
    assert_eq!(g.seen, ["pause", "enter"], "one pause, one resume");
    drop(g);
    let keys = keys_pressed(&rig);
    assert_eq!(keys.len(), 1, "{keys:#?}");
    assert!(keys[0].contains("Resume\\x20goal"), "{}", keys[0]);
    let h = hold(&rig).expect("kept");
    assert_eq!(
        (h.stage, h.why.as_str(), h.relaunched),
        (Stage::Resumed, "moved", rig.state().resumed_pid)
    );
}

/// THE RELAUNCH'S BOX STILL STANDING AT A LATER LOOK is the upgrade's to
/// answer too (the goal-pause review of 2026-09-28: past the relaunch's
/// first moments no look of the upgrade's pressed it — `goal_look` read no
/// footer under the box and waited — and the resume hung on the approval
/// policy's full power alone). The move made (the Codex current in the tab
/// is aterm's relaunch), the goal paused, its box up: ONE guarded Enter on
/// its focused resume, the record `Resumed` (`moved`) and naming the
/// relaunch. NEGATIVE CONTROL: the box left to a person (`BOX_THEIRS`) —
/// nothing pressed, the resume waited on.
#[test]
fn the_relaunchs_box_standing_at_a_later_look_is_answered() {
    for theirs in [false, true] {
        let (mut rig, goal) = goal_rig(&format!("goal-late-box-{theirs}"), true);
        rig.tui.version = v("0.157.1");
        rig.tui.pid = NEW;
        save(
            &rig.opts,
            &key(TAB),
            &St {
                phase: Phase::Done,
                relaunched_pid: NEW,
                resumed_pid: NEW,
                ..rig.state()
            },
        );
        let now = now_s();
        let mut paused = Hold {
            stage: Stage::Paused,
            took_at: now - 600,
            ..Hold::pausing(Owner::Upgrade, How::Typed, TUI, "0.157.1", now - 900)
        };
        if theirs {
            paused.say(cx::BOX_THEIRS);
        }
        write_hold(&rig, &paused);
        {
            let mut g = goal.lock().expect("goal");
            g.paused = true;
            g.boxed = true;
        }
        let r = rig.visit(None);
        let keys = keys_pressed(&rig);
        let h = hold(&rig).expect("kept");
        if theirs {
            assert_eq!(r.step, "wait:goal-resume", "{r:?}");
            assert!(keys.is_empty(), "{keys:#?}");
            assert_eq!(h.stage, Stage::Paused);
        } else {
            assert_eq!(r.step, "goal-resumed:moved", "{r:?}\n{:#?}", rig.asked());
            assert_eq!(keys.len(), 1, "{keys:#?}");
            assert!(keys[0].contains("Resume\\x20goal") && keys[0].ends_with(" enter"));
            assert_eq!(
                (h.stage, h.why.as_str(), h.relaunched),
                (Stage::Resumed, "moved", NEW)
            );
        }
        assert!(lines(&rig).is_empty(), "nothing typed");
    }
}

/// A GOAL IS NEVER RESUMED WITHOUT ITS MOVE INSIDE A SANDBOX (the goal-pause
/// review of 2026-09-28; the owner: "not continue work in a sandbox"): the
/// goal paused, its hold past its hour with no move, and its thread's last
/// turn run in `workspace-write` while the Codex's launch bypassed the
/// sandbox (Codex's own records, the switch's reader): nothing is typed —
/// the goal stays paused (`wait:goal-sandboxed`), said ONCE with the fix (a
/// relaunch with the launch's flags). NEGATIVE CONTROL: the thread outside
/// the sandbox (`danger-full-access`) — resumed at its bound, as before.
#[test]
fn a_goal_is_never_resumed_without_its_move_inside_a_sandbox() {
    for (sandbox, withheld) in [("workspace-write", true), ("danger-full-access", false)] {
        let (mut rig, goal) = goal_rig(&format!("goal-sandbox-{withheld}"), true);
        rig.tui
            .argv
            .push("--dangerously-bypass-approvals-and-sandbox".to_string());
        assert_eq!(rig.visit(None).step, "goal-paused");
        turn_ends(&rig);
        rollout_push(
            &rig,
            &[&format!(
                r#"{{"type":"turn_context","payload":{{"cwd":"/w","sandbox_policy":{{"type":"{sandbox}"}}}}}}"#
            )],
        );
        let h = hold(&rig).expect("held");
        let bound = cx::GOAL_HOLD_BOUND_S + 5;
        write_hold(
            &rig,
            &Hold {
                at: h.at - bound,
                tried_at: h.tried_at - bound,
                took_at: h.took_at - bound,
                ..h
            },
        );
        assert!(goal.lock().expect("goal").paused);
        let r = rig.visit(None);
        let h = hold(&rig).expect("kept");
        let steps = rig.ledger_steps();
        if withheld {
            assert_eq!(r.step, "wait:goal-sandboxed", "{r:?}");
            assert_eq!(lines(&rig), ["/goal pause"], "nothing more typed");
            assert_eq!(h.stage, Stage::Paused, "still owed");
            assert!(h.said("sandboxed"));
            assert_eq!(rig.visit(None).step, "wait:goal-sandboxed");
            let said = rig
                .ledger_steps()
                .iter()
                .filter(|s| *s == "wait:goal-sandboxed")
                .count();
            assert_eq!(said, 1, "said once: {steps:?}");
        } else {
            assert_eq!(r.step, "goal-resumed:bound", "{r:?}");
            assert_eq!(lines(&rig), ["/goal pause", "/goal resume"]);
        }
    }
}
