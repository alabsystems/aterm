// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The walls past W2 (`--mode wall-shift`, `wall-redraw`, `wall-switch`,
//! `wall-unread`, `wall-day-stop`, `wall-cruise`, `wall-relaunch`,
//! `wall-bucket`): the lab's drive of the stub and the tab as pure
//! decisions, and THE JUDGES ([`super::walls`]) over records each passes,
//! each beside the negative controls a broken product's records fail.

use super::*;

const MUSE_LIMITED: &str = "EVENT limited seq=2 message=◆ Usage limit reached · /upgrade (x) for increased limits, or wait for usage to reset at 1:06 PM reset=1:06 PM";

/// A wall raised at +6 s that resets at `reset` ms after [`T0`].
fn wall_at(reset: i64) -> Option<(i64, i64)> {
    Some((T0 + 6_000, (T0 + reset) / 1000))
}

fn approved(t: i64) -> Row {
    row(
        t,
        "limit-retry@v1",
        "approved",
        "Keep trying (reset 5:06 AM)",
        "",
    )
}

fn stub_read() -> Option<StubSeen> {
    Some(StubSeen {
        baseline: 2,
        read: 0,
        new_turns: 0,
    })
}

fn window_read() -> Option<StubSeen> {
    Some(StubSeen {
        baseline: 1,
        read: 3,
        new_turns: 0,
    })
}

/// A `WAITING … rule=limit-retry@v1` of seq `seq` at `t` until `until`.
fn retry_wait(t: i64, seq: u32, until: i64) -> (i64, String) {
    j(
        t,
        &format!(
            "WAITING seq={seq} until={} rule=limit-retry@v1 Gemini CLI's usage limit resets at 5:06 AM: Keep trying is pressed past it",
            stamp(until)
        ),
    )
}

// ---- the lab's drive, as decisions

/// THE STUB'S WALL RAISED AGAIN AT ITS RESET ([`wall_move`]): nothing until
/// [`WALL_LIFT_EARLY_MS`] before the reset standing; there G3's chain raises
/// it again [`SHIFT_STEP_MS`] on, twice, then lifts it; W4's once; every
/// other walled chain lifts it at once; W5's is lifted by its switch alone;
/// nothing once lifted or before any wall. NEGATIVE CONTROLS: a step before
/// the early lift holds, and a chain past its shifts lifts.
#[test]
fn the_wall_is_raised_again_at_its_reset_as_each_chain_shifts() {
    let reset = 1_300;
    let mut c = Cover {
        wall: Some((T0, reset)),
        ..Cover::default()
    };
    let due = reset * 1000 - WALL_LIFT_EARLY_MS;
    assert_eq!(wall_move(Chain::ShiftWall, &c, due - 1), WallMove::Hold);
    let step = SHIFT_STEP_MS / 1000;
    assert_eq!(
        wall_move(Chain::ShiftWall, &c, due),
        WallMove::Shift(reset + step)
    );
    assert_eq!(
        wall_move(Chain::RedrawWall, &c, due),
        WallMove::Shift(reset + REDRAW_AHEAD_MS / 1000)
    );
    for chain in [
        Chain::Wall,
        Chain::WallHid,
        Chain::WallDay,
        Chain::WallClimb,
        Chain::UnreadWall,
    ] {
        assert_eq!(wall_move(chain, &c, due), WallMove::Lift, "{chain:?}");
    }
    assert_eq!(
        wall_move(Chain::SwitchWall, &c, due + 3_600_000),
        WallMove::Hold
    );
    // one shift made: the next is due before the new reset, not the old one
    c.shifted.push((due, reset + step));
    assert_eq!(
        wall_move(Chain::ShiftWall, &c, due + 10_000),
        WallMove::Hold
    );
    let due2 = (reset + step) * 1000 - WALL_LIFT_EARLY_MS;
    assert_eq!(
        wall_move(Chain::ShiftWall, &c, due2),
        WallMove::Shift(reset + 2 * step)
    );
    assert_eq!(wall_move(Chain::RedrawWall, &c, due2), WallMove::Lift);
    c.shifted.push((due2, reset + 2 * step));
    let due3 = (reset + 2 * step) * 1000 - WALL_LIFT_EARLY_MS;
    assert_eq!(wall_move(Chain::ShiftWall, &c, due3), WallMove::Lift);
    c.lifted_ms = Some(due3);
    assert_eq!(wall_move(Chain::ShiftWall, &c, due3 + 1), WallMove::Hold);
    assert_eq!(
        wall_move(Chain::ShiftWall, &Cover::default(), due),
        WallMove::Hold
    );
    // each reset on a whole minute: the dialog's and the notices' minute
    assert_eq!(SHIFT_STEP_MS % 60_000, 0);
    assert_eq!(REDRAW_AHEAD_MS % 60_000, 0);
    assert_eq!(
        [Chain::ShiftWall, Chain::RedrawWall, Chain::Wall].map(Chain::shifts),
        [SHIFT_TIMES, 1, 0]
    );
}

/// THE RESET A REQUEST WAS ANSWERED WITH ([`Cover::reset_at`]): none before
/// the wall, the first wall's until the first raise again, each raise's from
/// it on, none from the lift; [`Cover::reset_now`] the last raised. The
/// window no harness text may reach the stub in ends where each chain's wall
/// does ([`wall_window_end`]).
#[test]
fn a_requests_reset_is_the_wall_standing_when_it_was_answered() {
    let mut c = Cover {
        wall: Some((T0, 1_300)),
        shifted: vec![(T0 + 297_000, 1_600), (T0 + 597_000, 1_900)],
        lifted_ms: Some(T0 + 897_000),
        ..Cover::default()
    };
    assert_eq!(c.reset_at(T0 - 1), None);
    assert_eq!(c.reset_at(T0 + 100_000), Some(1_300));
    assert_eq!(c.reset_at(T0 + 297_000), Some(1_600));
    assert_eq!(c.reset_at(T0 + 600_000), Some(1_900));
    assert_eq!(c.reset_at(T0 + 897_000), None);
    assert_eq!(c.reset_now(), Some(1_900));
    assert_eq!(wall_window_end(Chain::ShiftWall, &c), Some(1_900_000));
    assert_eq!(wall_window_end(Chain::RedrawWall, &c), Some(1_300_000));
    assert_eq!(wall_window_end(Chain::Wall, &c), Some(1_300_000));
    assert_eq!(wall_window_end(Chain::SwitchWall, &c), None);
    c.switch = Some(Switch {
        exited_ms: T0 + 200_000,
        ..Switch::default()
    });
    assert_eq!(wall_window_end(Chain::SwitchWall, &c), Some(T0 + 200_000));
    assert_eq!(wall_window_end(Chain::UnreadWall, &c), Some(1_300_000));
    c.unread = Some(Unread {
        restored_ms: Some(T0 + 4_000_000),
        ..Unread::default()
    });
    assert_eq!(wall_window_end(Chain::UnreadWall, &c), Some(T0 + 4_000_000));
    assert_eq!(wall_window_end(Chain::Wall, &Cover::default()), None);
}

/// W5's SECOND AGENT ([`switch_to`]) is never the first, and both are agents
/// whose limits the loop reads and decides on; each agent's exit is the one
/// measured for it ([`exit_of`]); the tab's program is read off `ls`
/// ([`program_of`]) and a shell is the agent gone ([`is_shell`]).
#[test]
fn the_switch_starts_another_agent_once_the_first_has_left_its_tab() {
    for h in Harness::ALL {
        assert_ne!(switch_to(h), h, "{h:?}");
    }
    assert_eq!(
        Harness::ALL.map(switch_to),
        [Harness::Muse, Harness::Muse, Harness::Muse, Harness::Claude]
    );
    assert_eq!(
        Harness::ALL.map(exit_of),
        [
            Exit::CancelThenCtrlCTwice(CLAUDE_CANCEL),
            Exit::OptionThenCommand(GEMINI_HIDE_OPTION, "/quit"),
            Exit::Command("/quit"),
            Exit::CtrlCTwice
        ]
    );
    // Claude Code's countdown, as `y4sx-claude` drew it: armed until its
    // cancel is drawn, the armed row staying above the cancel
    let armed = "⏺ Usage limit reached · continuing automatically at 10:15am · esc to cancel\n❯ ";
    assert!(countdown_armed(armed, CLAUDE_CANCEL));
    let cancelled =
        format!("{armed}\n⏺ Automatic continue cancelled · /rate-limit-options to re-arm\n❯ ");
    assert!(!countdown_armed(&cancelled, CLAUDE_CANCEL));
    assert!(!countdown_armed("❯ ", CLAUDE_CANCEL));
    let ls = "OK 2\n  1 w1 s-1 program=-zsh agent=- title=a\n  2 w1 s-2 program=claude agent=prompt title=b\n";
    assert_eq!(program_of(ls, "s-1").as_deref(), Some("zsh"));
    assert_eq!(program_of(ls, "s-2").as_deref(), Some("claude"));
    assert_eq!(program_of(ls, "s-3"), None);
    assert!(is_shell("zsh") && is_shell("sh") && is_shell("bash"));
    assert!(!is_shell("claude") && !is_shell("node") && !is_shell("muse"));
    // watched past the first agent's reset by the longest grace, a minute more
    assert_eq!(switch_watch_until(1_000), 1_000_000 + 150_000 + 60_000);
}

/// W6's TRANSCRIPT ASIDE ([`aside_of`]) is beside it under a name no reader
/// of `*.jsonl` takes; THE TOLD is read off its `EXCEPTION` line
/// ([`told_unread`]: the loop's own words for the unread limit), and the
/// conversation is read again a minute after it, or, with none,
/// [`UNREAD_TOLD_LATE_MS`] past its due ([`unread_restore_due`]). NEGATIVE
/// CONTROLS: another exception's line, or one before the move's time, is no
/// told; a minute short of either bound reads nothing again.
#[test]
fn the_unread_conversation_is_read_again_after_the_told() {
    let f = PathBuf::from("/x/projects/-p/abc.jsonl");
    assert_eq!(
        aside_of(&f),
        PathBuf::from("/x/projects/-p/abc.jsonl.unread")
    );
    assert_ne!(
        aside_of(&f).extension().and_then(|e| e.to_str()),
        Some("jsonl")
    );
    assert_eq!(futile_after_ms(), 3_600_000);
    let told_line = format!(
        "EXCEPTION seq=40 kind=futile when=later {} => x",
        unread_told_words()
    );
    let u = Unread {
        file: f,
        moved_ms: T0 + 215_000,
        restored_ms: None,
    };
    let reset_s = (T0 + 420_000) / 1000;
    let due = T0 + 420_000 + 3_600_000;
    let mut s = Seen::default();
    s.journal.push(j(100_000, &told_line));
    s.journal.push(j(
        4_000_000,
        "EXCEPTION seq=39 kind=spend when=later spent => x",
    ));
    assert_eq!(told_unread(&s, u.moved_ms), None);
    assert!(!unread_restore_due(
        &s,
        &u,
        reset_s,
        due + UNREAD_TOLD_LATE_MS - 1
    ));
    assert!(unread_restore_due(
        &s,
        &u,
        reset_s,
        due + UNREAD_TOLD_LATE_MS
    ));
    s.journal.push(j(4_020_500, &told_line));
    assert_eq!(told_unread(&s, u.moved_ms), Some(T0 + 4_020_500));
    let back = T0 + 4_020_500 + UNREAD_RESTORE_AFTER_TOLD_MS;
    assert!(!unread_restore_due(&s, &u, reset_s, back - 1));
    assert!(unread_restore_due(&s, &u, reset_s, back));
}

/// THE FOUR MODES: each its own chain on a lab of its own and its own
/// scenario; G3's on Gemini CLI alone, W6's on Claude Code alone (the others
/// say why not), W4's and W5's on every harness; W4's and W6's tabs hidden
/// as W2's (W4's only where the resume waits on it); every chain lands,
/// under Land, scripted as W1.
#[test]
fn the_walls_past_w2_each_run_their_own_chain_where_they_reach() {
    let modes = [
        (
            "wall-shift",
            Mode::WallShift,
            Chain::ShiftWall,
            Scenario::W3,
        ),
        (
            "wall-redraw",
            Mode::WallRedraw,
            Chain::RedrawWall,
            Scenario::W4,
        ),
        (
            "wall-switch",
            Mode::WallSwitch,
            Chain::SwitchWall,
            Scenario::W5,
        ),
        (
            "wall-unread",
            Mode::WallUnread,
            Chain::UnreadWall,
            Scenario::W6,
        ),
    ];
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    for (w, m, c, sc) in modes {
        assert_eq!(Mode::of(w), Some(m));
        assert_eq!(m.walled(), Some(sc));
        assert_eq!(Chain::of_mode(m), [c]);
        assert_eq!(c.scenarios(), [sc]);
        assert!(c.walled() && c.lands() && !c.managed() && c.autopilot() == "land");
        assert!(Chain::every().contains(&c));
        assert!(c.limit_ms() > c.wall_ahead_ms());
        let a = parse_args(&v(&["--harness", "gemini", "--mode", w]))
            .unwrap()
            .unwrap();
        assert_eq!(a.mode, m);
        // scripted as W1: the work reply, then the clean defense to the act
        assert_eq!(reply(c, &[]).as_deref(), Some(WORK));
        assert_eq!(reply(c, &[LAND]).as_deref(), Some(CLEAN));
        let resume = format!("{MARK} carry on");
        assert_eq!(reply(c, &[LAND, &resume]).as_deref(), Some(CLEAN));
        assert!(!sc.what().is_empty());
    }
    let reach = |m: Mode| Harness::ALL.map(|h| wall_out_of_reach(h, m).is_none());
    assert_eq!(reach(Mode::WallShift), [false, true, false, false]);
    assert_eq!(reach(Mode::WallRedraw), [true, true, true, true]);
    assert_eq!(reach(Mode::WallSwitch), [true, true, true, true]);
    assert_eq!(reach(Mode::WallUnread), [true, false, false, false]);
    assert_eq!(
        Chain::of_mode_on(Mode::WallShift, Harness::Muse),
        Vec::<Chain>::new()
    );
    assert_eq!(
        Chain::of_mode_on(Mode::WallUnread, Harness::Claude),
        [Chain::UnreadWall]
    );
    assert_eq!(
        Harness::ALL.map(|h| Chain::RedrawWall.hides_on(h)),
        [true, true, false, false]
    );
    assert!(Harness::ALL.iter().all(|&h| Chain::UnreadWall.hides_on(h)
        && Chain::WallHid.hides_on(h)
        && !Chain::ShiftWall.hides_on(h)
        && !Chain::SwitchWall.hides_on(h)
        && !Chain::Wall.hides_on(h)));
    assert_eq!(
        [Scenario::W3, Scenario::W4, Scenario::W5, Scenario::W6].map(Scenario::id),
        ["W3", "W4", "W5", "W6"]
    );
    // the told's words are the loop's own
    assert_eq!(
        unread_told_words(),
        aterm_agent::supervise::policy::exception::Exception::unread_limit().why
    );
}

// ---- W3: G3's climb across a changed reset

/// W3's records on Gemini CLI (`--mode wall-shift`): the land act at +125 s
/// met the account dialog (reset +300 s); the lab raised the wall again at
/// +297 s (reset +600 s) and at +597 s (reset +900 s) and lifted it at +897 s.
/// Try 1 at +420 s met the wall again and its new reset was waited on; try 2
/// at +720 s the same; try 3 at +1020 s got through; landed at +1024 s.
fn gemini_shifted() -> Seen {
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(
                140_000,
                "EVENT prompt seq=2 kind=usage-limit classify=- command=-",
            ),
            retry_wait(140_200, 2, 419_000),
            j(420_000, "APPROVED seq=2 Keep trying (reset 5:06 AM)"),
            j(
                422_500,
                "EVENT prompt seq=3 kind=usage-limit classify=- command=-",
            ),
            retry_wait(423_000, 3, 719_000),
            j(720_000, "APPROVED seq=3 Keep trying (reset 5:11 AM)"),
            j(
                722_500,
                "EVENT prompt seq=4 kind=usage-limit classify=- command=-",
            ),
            retry_wait(723_000, 4, 1_019_000),
            j(1_020_000, "APPROVED seq=4 Keep trying (reset 5:16 AM)"),
            j(1_024_000, "EVENT idle seq=5 Nothing left: the parser"),
            j(1_024_000, "DONE seq=5 rule=land@v1 x"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            point(140_000, 2),
            approved(420_000),
            approved(720_000),
            approved(1_020_000),
            point(1_024_000, 3),
            task_done(1_024_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(1_025_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 1_024_000 + 545_000,
        stub: stub_read(),
        cover: Cover {
            wall: wall_at(300_000),
            shifted: vec![
                (T0 + 297_000, (T0 + 600_000) / 1000),
                (T0 + 597_000, (T0 + 900_000) / 1000),
            ],
            lifted_ms: Some(T0 + 897_000),
            window_stub: window_read(),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// G3'S CLIMB ACROSS A CHANGED RESET PASSES W3: each try came past the reset
/// its dialog named, the two that met the wall again were each waited on to
/// the new reset, the third got through, nothing typed, landed. NEGATIVE
/// CONTROLS: a try before the reset its dialog named (the loop pressing on
/// the old reset), a dialog drawn again with no wait on its new reset, a
/// text typed into the dialog, a try past the deadline, no try past the
/// last reset, a press after the one that got through, and an escalation
/// each FAIL; a wall lifted after one raise (one try in the wall) did not
/// drive the climb: UNOBSERVED.
#[test]
fn g3s_climb_across_a_changed_reset_waits_each_new_reset_out() {
    let w3 = |s: &Seen| one(Chain::ShiftWall, s, "gemini", Scenario::W3);
    let (w, why) = w3(&gemini_shifted());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("try 1 at +420.0 s (120.0 s past the reset +300.0 s) met the wall again, its dialog's new reset +600.0 s waited on from +423.0 s")
            && why.contains("try 2 at +720.0 s (120.0 s past the reset +600.0 s) met the wall again")
            && why.contains("try 3 at +1020.0 s (120.0 s past the reset +900.0 s) got through")
            && why.contains("DONE at +1024.0 s"),
        "{why}"
    );
    let fail = |s: &Seen, needle: &str| {
        let (w, why) = w3(s);
        assert_eq!(w, "FAIL", "{needle}: {why}");
        assert!(why.contains(needle), "{needle}: {why}");
    };
    // the loop pressed on the reset the first dialog named, not the new one
    let mut s = gemini_shifted();
    s.ledger[4].ts = T0 + 590_000;
    fail(&s, "PRESSED BEFORE THE RESET: try 2 at +590.0 s");
    // the dialog drawn again with a new reset, and no wait on it
    let mut s = gemini_shifted();
    s.journal.retain(|(t, _)| *t != T0 + 423_000);
    fail(
        &s,
        "try 1 at +420.0 s met the wall again (its new reset +600.0 s), but no wait on that reset",
    );
    // ... or a wait still on the reset the first dialog named: the change unread
    let mut s = gemini_shifted();
    for (t, l) in &mut s.journal {
        if *t == T0 + 423_000 {
            *l = retry_wait(423_000, 3, 419_000).1;
        }
    }
    fail(
        &s,
        "try 1 at +420.0 s met the wall again (its new reset +600.0 s), but no wait on that reset",
    );
    // a text typed into the dialog
    let mut s = gemini_shifted();
    s.journal.insert(
        8,
        j(
            700_000,
            "CONTINUED seq=3 rule=usage-resume@v1 [aterm harness] Still landing:",
        ),
    );
    fail(&s, "TYPED into the dialog: usage-resume@v1 at +700.0 s");
    // the last try past its deadline (the reset, its minute, the grace, 5 min)
    let mut s = gemini_shifted();
    s.ledger[5].ts = T0 + 1_400_000;
    fail(&s, "PERSISTENCE LOST: try 3 at +1400.0 s");
    // no try past the last reset, watched long past it
    let mut s = gemini_shifted();
    s.ledger.truncate(5);
    s.journal.truncate(11);
    s.end_ms = T0 + 1_400_000;
    fail(&s, "PERSISTENCE LOST: no try got through by +1400.0 s");
    // a press after the try that got through
    let mut s = gemini_shifted();
    s.ledger.insert(6, approved(1_022_000));
    fail(
        &s,
        "a press at +1022.0 s after the try that got through at +1020.0 s",
    );
    // the wall handed to a person while it stood
    let mut s = gemini_shifted();
    s.ledger
        .insert(3, row(140_100, "-", "escalated", "Keep trying", "x"));
    fail(&s, "ESCALATED the wall to a person at +140.1 s");
    // the lab lifted the wall after one raise: one try in the wall, the
    // second got through
    let mut s = gemini_shifted();
    s.cover.shifted.truncate(1);
    s.cover.lifted_ms = Some(T0 + 597_000);
    s.ledger.remove(5);
    s.journal
        .retain(|(t, _)| *t < T0 + 722_000 || *t >= T0 + 1_024_000);
    let (w, why) = w3(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("only 1 of 2 tries met the wall again"),
        "{why}"
    );
}

// ---- W4: the redraw after a typed resume

/// W4's records on Muse Code (`--mode wall-redraw`): the land act at +125 s
/// met the wall (reset +300 s); the lab raised it again at +297 s (reset
/// +600 s) and lifted it at +597 s; the resume typed at +361 s met it, the
/// notice drawn again at +366 s and waited on to the new reset; the resume
/// at +661 s landed at +665 s.
fn muse_redrawn() -> Seen {
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(140_000, MUSE_LIMITED),
            j(
                140_100,
                &format!(
                    "WAITING seq=2 until={} usage-session resets",
                    stamp(300_000)
                ),
            ),
            j(
                361_000,
                "CONTINUED seq=2 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(366_000, &MUSE_LIMITED.replace("seq=2", "seq=3")),
            j(
                366_100,
                &format!(
                    "WAITING seq=3 until={} usage-session resets",
                    stamp(600_000)
                ),
            ),
            j(
                661_000,
                "CONTINUED seq=3 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(665_000, "EVENT idle seq=4 Nothing left: the parser"),
            j(665_000, "DONE seq=4 rule=land@v1 x"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            point(140_000, 2),
            typed(361_000, "usage-resume@v1", LAND_NUDGE),
            point(366_000, 3),
            typed(661_000, "usage-resume@v1", LAND_NUDGE),
            point(665_000, 4),
            task_done(665_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(666_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        // the landing closed four short turns: its window is 16 min and 60 s
        end_ms: T0 + 665_000 + 1_025_000,
        stub: stub_read(),
        cover: Cover {
            wall: wall_at(300_000),
            shifted: vec![(T0 + 297_000, (T0 + 600_000) / 1000)],
            lifted_ms: Some(T0 + 597_000),
            window_stub: window_read(),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// THE REDRAW AFTER A TYPED RESUME PASSES W4: the resume past the first reset
/// met the wall again, the notice read again, nothing typed into the redrawn
/// wall, the resume past the new reset landed; on Claude Code its own
/// continuation past the new reset is going on too. NEGATIVE CONTROLS: a text
/// typed into the first wall or into the redrawn one, a resume past its
/// deadline, an act on top of the program's own continuation, and a landing
/// before the lift each FAIL; a wall never raised again, or a resume no limit
/// point followed, did not drive the redraw: UNOBSERVED.
#[test]
fn a_typed_resume_that_meets_the_wall_again_waits_the_new_reset_out() {
    let w4 = |s: &Seen, p: &str| one(Chain::RedrawWall, s, p, Scenario::W4);
    let (w, why) = w4(&muse_redrawn(), "muse");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("usage-resume@v1 at +361.0 s (61.0 s past the first reset)")
            && why.contains("the wall drawn again +366.0 s")
            && why.contains("gone on: usage-resume@v1 at +661.0 s (61.0 s past the new reset; the loop's floor after a resume that met the wall again: +996.0 s)"),
        "{why}"
    );
    let fail = |s: &Seen, p: &str, needle: &str| {
        let (w, why) = w4(s, p);
        assert_eq!(w, "FAIL", "{needle}: {why}");
        assert!(why.contains(needle), "{needle}: {why}");
    };
    // typed into the redrawn wall
    let mut s = muse_redrawn();
    s.journal.insert(
        8,
        j(
            400_000,
            "CONTINUED seq=3 rule=usage-resume@v1 [aterm harness] Still landing:",
        ),
    );
    s.ledger
        .insert(5, typed(400_000, "usage-resume@v1", LAND_NUDGE));
    fail(
        &s,
        "muse",
        "TYPED into the redrawn wall: usage-resume@v1 at +400.0 s",
    );
    // typed into the first wall
    let mut s = muse_redrawn();
    s.journal[5].0 = T0 + 250_000;
    s.ledger[3].ts = T0 + 250_000;
    fail(
        &s,
        "muse",
        "TYPED into the wall: usage-resume@v1 at +250.0 s",
    );
    // the resume past its deadline: the loop's floor after the resume that
    // met the wall again (+366 s, the take window's 30 s, 10 min), 5 min more
    let mut s = muse_redrawn();
    for (t, _) in s.journal.iter_mut().skip(8) {
        *t += 700_000;
    }
    for r in s.ledger.iter_mut().skip(5) {
        r.ts += 700_000;
    }
    s.status[1].0 += 700_000;
    s.end_ms += 700_000;
    fail(&s, "muse", "PERSISTENCE LOST: usage-resume@v1 at +1361.0 s");
    // landed while the redrawn wall stood
    let mut s = muse_redrawn();
    s.journal.insert(8, j(500_000, "DONE seq=3 rule=land@v1 x"));
    fail(&s, "muse", "LANDED at +500.0 s while the wall stood");
    // Claude Code going on by itself past the new reset, nothing typed: going on
    let mut s = muse_redrawn();
    s.journal.remove(8);
    s.ledger.remove(5);
    s.cover.went_on_ms = Some(T0 + 640_000);
    let (w, why) = w4(&s, "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("gone on: the program's own continuation at +640.0 s"),
        "{why}"
    );
    // ... and the harness's resume typed on top of it
    let mut s = muse_redrawn();
    s.cover.went_on_ms = Some(T0 + 640_000);
    fail(
        &s,
        "claude",
        "TYPED ON TOP of the program's own continuation",
    );
    // never raised again: no redraw to meet
    let mut s = muse_redrawn();
    s.cover.shifted.clear();
    let (w, why) = w4(&s, "muse");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never raised again"), "{why}");
    // the resume never met the wall again on the screen
    let mut s = muse_redrawn();
    s.journal.remove(6);
    let (w, why) = w4(&s, "muse");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("the redraw was not seen"), "{why}");
}

/// W4's records on agy as measured (`y6rd-agy`): the land act at +125.1 s
/// met the wall (reset +464.1 s); the lab raised it again at +462.4 s (reset
/// +764.1 s) and lifted it at +762.6 s; the resume typed at +553.8 s met it,
/// the notice drawn again at +554.3 s, the take window to +583.6 s, then the
/// loop waited on its floor after a resume that met the wall again (10 min),
/// past the new reset; that resume at +1185.0 s landed at +1188.4 s.
fn agy_redrawn() -> Seen {
    let agy_limited = |seq: u32, secs: u32| {
        format!(
            "EVENT limited seq={seq} message=agent executor error: generating and executing: Error 429, Message: You exceeded your current quota reset=in {secs}s"
        )
    };
    Seen {
        journal: vec![
            j(3_600, "EVENT idle seq=1 Implemented the parser"),
            j(
                4_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_100,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(125_600, &agy_limited(2, 339)),
            j(
                125_600,
                &format!(
                    "WAITING seq=2 until={} the worker to take the land@v1 act",
                    stamp(155_600)
                ),
            ),
            j(
                155_600,
                &format!("WAITING seq=2 until={} spend resets", stamp(553_000)),
            ),
            j(
                553_800,
                "CONTINUED seq=2 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(554_300, &agy_limited(3, 211)),
            j(
                554_300,
                &format!(
                    "WAITING seq=3 until={} the worker to take the usage-resume@v1 act",
                    stamp(583_600)
                ),
            ),
            j(
                584_300,
                &format!("WAITING seq=3 until={} spend resets", stamp(1_183_600)),
            ),
            j(
                1_185_000,
                "CONTINUED seq=3 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(1_188_400, "EVENT idle seq=4 Nothing left: the parser"),
            j(1_188_400, "DONE seq=4 rule=land@v1 x"),
        ],
        ledger: vec![
            point(3_600, 1),
            typed(125_100, "land@v1", LAND),
            point(155_600, 2),
            typed(553_800, "usage-resume@v1", LAND_NUDGE),
            point(584_300, 3),
            typed(1_185_000, "usage-resume@v1", LAND_NUDGE),
            point(1_188_400, 4),
            task_done(1_188_400),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(1_189_500, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 1_188_400 + 1_025_000,
        stub: stub_read(),
        cover: Cover {
            wall: wall_at(464_100),
            shifted: vec![(T0 + 462_400, (T0 + 764_100) / 1000)],
            lifted_ms: Some(T0 + 762_600),
            window_stub: window_read(),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// W4 HOLDS THE GOING-ON PAST THE NEW RESET TO THE LOOP'S OWN DUE: after a
/// typed resume that met the wall again, the loop waits its floor
/// (`TurnEndTiming::limit_backoff`'s first rung) where that is later than the
/// new reset and its grace — the judge before this held it to the reset
/// alone and failed `y6rd-agy` as "PERSISTENCE LOST: usage-resume@v1 at
/// +1185.0 s, 421.0 s past the new reset". NEGATIVE CONTROLS: the resume
/// past that floor and [`WALL_LOST_MS`] FAILs; with the first going-on a
/// press of the dialog (no typed resume met the wall again) there is no
/// floor, and the same resume FAILs.
#[test]
fn the_redraw_after_a_typed_resume_is_waited_on_the_loops_floor() {
    let w4 = |s: &Seen| one(Chain::RedrawWall, s, "agy", Scenario::W4);
    let (w, why) = w4(&agy_redrawn());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("gone on: usage-resume@v1 at +1185.0 s (421.0 s past the new reset; the loop's floor after a resume that met the wall again: +1184.3 s)"),
        "{why}"
    );
    // past the floor and 5 min
    let mut s = agy_redrawn();
    for (t, _) in s.journal.iter_mut().skip(10) {
        *t += 300_000;
    }
    for r in s.ledger.iter_mut().skip(5) {
        r.ts += 300_000;
    }
    s.status[1].0 += 300_000;
    s.end_ms += 300_000;
    let (w, why) = w4(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("PERSISTENCE LOST: usage-resume@v1 at +1485.0 s"),
        "{why}"
    );
    // the first going-on a press, not a typed resume: no floor
    let mut s = agy_redrawn();
    s.journal.remove(6);
    s.ledger[3] = approved(553_800);
    let (w, why) = w4(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("PERSISTENCE LOST: usage-resume@v1 at +1185.0 s, 421.0 s past the new reset"),
        "{why}"
    );
}

// ---- W5: the cross-program end

/// W5's records (`--mode wall-switch`) on Claude Code, then Muse Code: the
/// land act at +125 s met Claude Code's limit (reset +1020 s), read and
/// waited on; the lab ended Claude Code at +200 s, lifted the wall, launched
/// Muse Code and prompted it at +220 s; Muse Code's work point at +230 s took
/// its own 2 min back-off, its land act at +351 s landed at +356 s; watched
/// past Claude Code's reset and the longest grace.
fn claude_switched() -> Seen {
    let reset = 1_020_000;
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(
                140_000,
                "EVENT limited seq=2 message=You've hit your session limit · resets 1:06pm reset=1:06pm",
            ),
            j(
                140_100,
                &format!(
                    "WAITING seq=2 until={} usage-session resets",
                    stamp(reset + 150_000)
                ),
            ),
            j(230_000, "EVENT idle seq=10 Implemented the parser"),
            j(
                230_100,
                "WAITING seq=10 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                351_000,
                "CONTINUED seq=10 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(356_000, "EVENT idle seq=11 Nothing left: the parser"),
            j(356_000, "DONE seq=11 rule=land@v1 x"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            point(140_000, 2),
            point(230_000, 1),
            typed(351_000, "land@v1", LAND),
            point(356_000, 2),
            task_done(356_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(357_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + reset + 220_000,
        stub: stub_read(),
        cover: Cover {
            wall: wall_at(reset),
            switch: Some(Switch {
                to: "muse",
                exited_ms: T0 + 200_000,
                shell_ms: T0 + 202_000,
                lifted_ms: Some(T0 + 203_000),
                launched_ms: T0 + 204_000,
                prompt_ms: T0 + 220_000,
            }),
            window_stub: window_read(),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// THE CROSS-PROGRAM END PASSES W5: Claude Code's limit held nothing of the
/// Muse Code after it — its point took its own back-off, its land act came
/// long before Claude Code's reset, it landed, and nothing was typed past
/// that reset. NEGATIVE CONTROLS (the remembered-limit stage's L1 as
/// measured before its fix): the second agent's point held unread on the
/// first's limit, its land act only past the first's reset, no land act at
/// all, a resume of the ended limit typed into it before its land act (T3 of
/// the review of walls-3: the one check that refuses it) or past the reset,
/// and a text typed into the first agent's wall each FAIL; no switch made, or
/// a watch short of the reset, is UNOBSERVED.
#[test]
fn another_agent_in_the_tab_is_not_held_by_the_first_agents_limit() {
    let w5 = |s: &Seen| one(Chain::SwitchWall, s, "claude", Scenario::W5);
    let (w, why) = w5(&claude_switched());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains(
            "muse's land act at +351.0 s (131.0 s after its prompt, 669.0 s before claude's reset): land@v1, due by +410.0 s"
        ) && why.contains("nothing typed past claude's reset"),
        "{why}"
    );
    let fail = |s: &Seen, needle: &str| {
        let (w, why) = w5(s);
        assert_eq!(w, "FAIL", "{needle}: {why}");
        assert!(why.contains(needle), "{needle}: {why}");
    };
    // held unread on the first agent's limit
    let mut s = claude_switched();
    s.journal.insert(
        6,
        j(
            230_050,
            &format!(
                "WAITING seq=10 until={} {UNREAD_WORDS}; no act on its account",
                stamp(530_050)
            ),
        ),
    );
    fail(&s, "claude's LIMIT HELD muse: +230.1 s");
    // its land act only past the first agent's reset
    let mut s = claude_switched();
    for (t, _) in s.journal.iter_mut().skip(7) {
        *t += 700_000;
    }
    for r in s.ledger.iter_mut().skip(4) {
        r.ts += 700_000;
    }
    s.status[1].0 += 700_000;
    s.end_ms += 700_000;
    fail(&s, "its land act at +1051.0 s came past claude's reset");
    // no land act at all, watched past its due
    let mut s = claude_switched();
    s.journal.truncate(7);
    s.ledger.truncate(4);
    fail(&s, "claude's LIMIT HELD muse: no land act");
    // the ended limit's resume typed into the second agent before its land
    // act, all of it before the first agent's reset (its own land act and
    // landing come after)
    let mut s = claude_switched();
    s.journal[7] = j(
        351_000,
        "CONTINUED seq=10 rule=usage-resume@v1 [aterm harness] Still landing:",
    );
    s.journal[8] = j(356_000, "EVENT idle seq=11 Implemented the parser");
    s.journal[9] = j(
        597_000,
        "CONTINUED seq=11 rule=land@v1 [aterm harness] Autopilot is Land:",
    );
    s.journal
        .push(j(602_000, "EVENT idle seq=12 Nothing left: the parser"));
    s.journal.push(j(602_000, "DONE seq=12 rule=land@v1 x"));
    s.ledger.truncate(4);
    s.ledger.extend([
        typed(351_000, "usage-resume@v1", LAND_NUDGE),
        point(356_000, 2),
        typed(597_000, "land@v1", LAND),
        point(602_000, 3),
        task_done(602_000),
    ]);
    s.status[1].0 = T0 + 603_000;
    fail(
        &s,
        "TYPED before muse's land act: usage-resume@v1 at +351.0 s",
    );
    // the ended limit's resume typed into the second agent past the reset
    let mut s = claude_switched();
    s.journal.push(j(
        1_175_000,
        "CONTINUED seq=12 rule=usage-resume@v1 [aterm harness] Still landing:",
    ));
    s.ledger
        .push(typed(1_175_000, "usage-resume@v1", LAND_NUDGE));
    fail(&s, "TYPED after the landing");
    // a text typed into the first agent's wall
    let mut s = claude_switched();
    s.journal.insert(
        5,
        j(
            160_000,
            "CONTINUED seq=2 rule=usage-resume@v1 [aterm harness] Still landing:",
        ),
    );
    fail(&s, "TYPED into claude's wall: usage-resume@v1 at +160.0 s");
    // no switch made
    let mut s = claude_switched();
    s.cover.switch = None;
    s.cover.switch_gave_up = Some("no muse in the lab (LAB_MUSE at up)".into());
    let (w, why) = w5(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("no switch made (no muse in the lab"), "{why}");
    // watched short of the first agent's reset
    let mut s = claude_switched();
    s.end_ms = T0 + 700_000;
    let (w, why) = w5(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("short of claude's reset"), "{why}");
}

/// W5 as measured on Gemini CLI then Muse Code (`y4sw`): the tab's episode
/// had Land's act typed already (the first agent's land act met the wall),
/// so the loop's act on the second agent is Land's nudge, at the back-off its
/// work point named (`2 short turn(s) in a row: 4 min back-off`) — past
/// one short turn's window. Muse Code's point at +230 s, the nudge at
/// +470.5 s, landed at +474 s.
fn muse_nudged() -> Seen {
    let mut s = claude_switched();
    s.journal[6] = j(
        230_100,
        "WAITING seq=10 until=x 2 short turn(s) in a row: 4 min back-off",
    );
    s.journal[7] = j(
        470_500,
        "CONTINUED seq=10 rule=land-nudge@v1 [aterm harness] Still landing:",
    );
    s.journal[8] = j(474_000, "EVENT idle seq=11 Nothing left: the parser");
    s.journal[9] = j(474_000, "DONE seq=11 rule=land@v1 x");
    s.ledger.truncate(4);
    s.ledger.extend([
        typed(470_500, "land-nudge@v1", LAND_NUDGE),
        point(474_000, 2),
        task_done(474_000),
    ]);
    s.status[1].0 = T0 + 475_000;
    s
}

/// W5's LAND ACT IS LAND'S RULE, AT THE BACK-OFF THE LOOP NAMED: Land's nudge
/// on the second agent is its land act, due at the back-off its point named
/// and [`PERSISTENCE_GRACE_MS`] (the judge before this read only `land@v1`
/// and failed `y4sw` as "TYPED before muse's land act: land-nudge@v1").
/// NEGATIVE CONTROLS: the nudge past that due, a resume typed before it, a
/// continuation of another rule in its place each FAIL; no land act watched
/// past one short turn's window but short of the named due is UNOBSERVED.
#[test]
fn the_second_agents_land_act_is_lands_nudge_at_the_back_off_the_loop_named() {
    let w5 = |s: &Seen| one(Chain::SwitchWall, s, "claude", Scenario::W5);
    let (w, why) = w5(&muse_nudged());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("muse's land act at +470.5 s (250.5 s after its prompt, 549.5 s before claude's reset): land-nudge@v1, due by +530.0 s"),
        "{why}"
    );
    let fail = |s: &Seen, needle: &str| {
        let (w, why) = w5(s);
        assert_eq!(w, "FAIL", "{needle}: {why}");
        assert!(why.contains(needle), "{needle}: {why}");
    };
    // the nudge past the named due and its grace
    let mut s = muse_nudged();
    for (t, _) in s.journal.iter_mut().skip(7) {
        *t += 70_000;
    }
    for r in s.ledger.iter_mut().skip(4) {
        r.ts += 70_000;
    }
    s.status[1].0 += 70_000;
    fail(
        &s,
        "its land act (land-nudge@v1) at +540.5 s came past its due +530.0 s (WAITING seq=10",
    );
    // the ended limit's resume typed before the nudge
    let mut s = muse_nudged();
    s.journal.insert(
        7,
        j(
            400_000,
            "CONTINUED seq=10 rule=usage-resume@v1 [aterm harness] Still landing:",
        ),
    );
    s.ledger
        .insert(4, typed(400_000, "usage-resume@v1", LAND_NUDGE));
    fail(
        &s,
        "TYPED before muse's land act: usage-resume@v1 at +400.0 s",
    );
    // another rule's continuation in the nudge's place
    let mut s = muse_nudged();
    s.journal[7] = j(
        470_500,
        "CONTINUED seq=10 rule=continue@v1 [aterm harness] Keep going",
    );
    s.ledger[4] = typed(470_500, "continue@v1", "Keep going");
    fail(&s, "TYPED before muse's land act: continue@v1 at +470.5 s");
    // no land act, watched past one short turn's window (+460 s) but short
    // of the named due and its grace (+530 s)
    let mut s = muse_nudged();
    s.journal.truncate(7);
    s.ledger.truncate(4);
    s.status.truncate(1);
    s.end_ms = T0 + 500_000;
    let (w, why) = w5(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains(
            "watched to +500.0 s, before muse's land act was due (+530.0 s: WAITING seq=10"
        ),
        "{why}"
    );
    // ... and watched past it, FAIL
    s.end_ms = T0 + 531_000;
    fail(
        &s,
        "claude's LIMIT HELD muse: no land act in the 311.0 s after its prompt (due by +530.0 s",
    );
}

// ---- W6: the unread limit told

/// W6's records on Claude Code (`--mode wall-unread`): the land act at +125 s
/// met the limit (reset +420 s); its notice out of sight at +210 s, the
/// transcript aside at +215 s, the limit held unread from +216.1 s; told at
/// +4020.5 s (an hour past the reset); the transcript back at +4081.5 s, the
/// told down and the resume typed at +4100 s, landed at +4104 s.
fn claude_held_unread() -> Seen {
    let told = format!(
        "EXCEPTION seq=40 kind=futile when=later {} => Type keep going",
        unread_told_words()
    );
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(
                140_000,
                "EVENT limited seq=2 message=You've hit your session limit · resets 1:06pm reset=1:06pm",
            ),
            j(
                140_100,
                &format!(
                    "WAITING seq=2 until={} usage-session resets",
                    stamp(570_000)
                ),
            ),
            j(216_000, "EVENT idle seq=31 -"),
            j(
                216_100,
                &format!(
                    "WAITING seq=31 until={} {UNREAD_WORDS}; no act on its account",
                    stamp(516_100)
                ),
            ),
            j(4_020_500, &told),
            j(
                4_099_900,
                "CLEARED seq=41 exception=futile after=79s the conversation read",
            ),
            j(
                4_100_000,
                "CONTINUED seq=41 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(4_104_000, "EVENT idle seq=42 Nothing left: the parser"),
            j(4_104_000, "DONE seq=42 rule=land@v1 x"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            point(140_000, 2),
            typed(4_100_000, "usage-resume@v1", LAND_NUDGE),
            point(4_104_000, 3),
            task_done(4_104_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(4_105_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 4_104_000 + 545_000,
        stub: stub_read(),
        cover: Cover {
            wall: wall_at(420_000),
            lifted_ms: Some(T0 + 417_000),
            hidden: Some(Hidden {
                first_ms: T0 + 156_000,
                enter_ms: T0 + 200_000,
                seen_ms: T0 + 210_000,
                seq: Some(30),
                commands: 12,
                command: CLAUDE_HIDE_COMMAND,
                stopped: false,
                words: &CLAUDE_WALL_WORDS,
            }),
            unread: Some(Unread {
                file: PathBuf::from("/x/abc.jsonl"),
                moved_ms: T0 + 215_000,
                restored_ms: Some(T0 + 4_081_500),
            }),
            window_stub: window_read(),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// THE UNREAD LIMIT TOLD PASSES W6: held unread with nothing typed, told an
/// hour past the reset with the hold standing, read again, the told down, the
/// resume typed, landed. NEGATIVE CONTROLS (the remembered-limit stage's L2:
/// two days on, nothing said): no told, a told before the hour, a told only
/// after the lab's late read-again (T4 of the review of walls-3), a text
/// typed while held, the told down while still held, the told standing past
/// the landing, and a resume past its deadline each FAIL; no unread hold
/// read, or a transcript never moved, did not make it unread: UNOBSERVED.
#[test]
fn a_limit_held_unread_an_hour_past_its_reset_is_told_and_still_held() {
    let w6 = |s: &Seen| one(Chain::UnreadWall, s, "claude", Scenario::W6);
    let (w, why) = w6(&claude_held_unread());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("held unread from +216.1 s")
            && why.contains("told at +4020.5 s (0.5 s past an hour past the reset), nothing typed")
            && why.contains("gone on: usage-resume@v1 at +4100.0 s")
            && why.contains("the told down at +4099.9 s"),
        "{why}"
    );
    let fail = |s: &Seen, needle: &str| {
        let (w, why) = w6(s);
        assert_eq!(w, "FAIL", "{needle}: {why}");
        assert!(why.contains(needle), "{needle}: {why}");
    };
    // nothing said: the limit held for good
    let mut s = claude_held_unread();
    s.journal.remove(7);
    fail(&s, "NOT TOLD");
    // told before the hour
    let mut s = claude_held_unread();
    s.journal[7].0 = T0 + 3_000_000;
    fail(&s, "TOLD EARLY at +3000.0 s");
    // told late: 660 s past the hour, after the transcript was put back,
    // and cleared before the landing
    let mut s = claude_held_unread();
    s.journal[7].0 = T0 + 4_680_000;
    s.journal[8].0 = T0 + 4_690_000;
    for (t, _) in s.journal.iter_mut().skip(9) {
        *t += 600_000;
    }
    for r in s.ledger.iter_mut().skip(3) {
        r.ts += 600_000;
    }
    s.status[1].0 += 600_000;
    s.end_ms += 600_000;
    if let Some(u) = s.cover.unread.as_mut() {
        u.restored_ms = Some(T0 + 4_620_000);
    }
    fail(&s, "TOLD LATE at +4680.0 s");
    // typed while held
    let mut s = claude_held_unread();
    s.journal.insert(
        7,
        j(
            1_000_000,
            "CONTINUED seq=35 rule=usage-resume@v1 [aterm harness] Still landing:",
        ),
    );
    s.ledger
        .insert(3, typed(1_000_000, "usage-resume@v1", LAND_NUDGE));
    fail(
        &s,
        "TYPED while the limit stood or was held unread: usage-resume@v1 at +1000.0 s",
    );
    // the told down while the limit was still held
    let mut s = claude_held_unread();
    s.journal.insert(
        8,
        j(4_050_000, "CLEARED seq=40 exception=futile after=30s x"),
    );
    fail(
        &s,
        "the told came down at +4050.0 s while the limit was still held",
    );
    // the told standing past the landing
    let mut s = claude_held_unread();
    s.journal.remove(8);
    fail(&s, "the told STOOD past the landing");
    // the resume past its deadline (the look, then 5 min)
    let mut s = claude_held_unread();
    for (t, _) in s.journal.iter_mut().skip(9) {
        *t += 700_000;
    }
    for r in s.ledger.iter_mut().skip(3) {
        r.ts += 700_000;
    }
    s.status[1].0 += 700_000;
    s.end_ms += 700_000;
    fail(&s, "PERSISTENCE LOST: usage-resume@v1 at +4800.0 s");
    // no unread hold read: the conversation was never unread to the loop
    let mut s = claude_held_unread();
    s.journal.remove(6);
    let (w, why) = w6(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("the loop never held the limit unread"),
        "{why}"
    );
    // the transcript never moved
    let mut s = claude_held_unread();
    s.cover.unread = None;
    s.cover.unread_gave_up = Some("no transcript holds FOREVERLAB-UNREAD-WALL.".into());
    let (w, why) = w6(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("no transcript holds"), "{why}");
}

// ---- the walls of the walls-3 stage's X1 and X2 (W7 to W10)

/// THE FOUR MODES OF X1 AND X2: each its own chain on a lab of its own and
/// its own scenario; W7's and W10's on Gemini CLI alone, W9's on the
/// screen-alone agents (Claude Code's limit is its conversation's), W8's on
/// every harness (the others say why not); W7's tab hidden as W2's (the
/// dialog answered Stop), on the per-day claim, W10's on the account's (a
/// reset named) with its CLI on the Pro model; W8 under Cruise, scripted as
/// Cruise (a done claim, its check answered clean, the resume answered with
/// the done claim again) and never landing; the others land, scripted as
/// W1.
#[test]
fn the_walls_of_x1_and_x2_each_run_their_own_chain_where_they_reach() {
    let modes = [
        (
            "wall-day-stop",
            Mode::WallDayStop,
            Chain::DayStopWall,
            Scenario::W7,
        ),
        (
            "wall-cruise",
            Mode::WallCruise,
            Chain::CruiseWall,
            Scenario::W8,
        ),
        (
            "wall-relaunch",
            Mode::WallRelaunch,
            Chain::RelaunchWall,
            Scenario::W9,
        ),
        (
            "wall-bucket",
            Mode::WallBucket,
            Chain::BucketWall,
            Scenario::W10,
        ),
    ];
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let resume = format!("{MARK} carry on");
    for (w, m, c, sc) in modes {
        assert_eq!(Mode::of(w), Some(m));
        assert_eq!(m.walled(), Some(sc));
        assert_eq!(Chain::of_mode(m), [c]);
        assert_eq!(c.scenarios(), [sc]);
        assert!(c.walled() && !c.managed(), "{c:?}");
        assert!(Chain::every().contains(&c));
        assert!(c.limit_ms() > c.wall_ahead_ms());
        let a = parse_args(&v(&["--harness", "gemini", "--mode", w]))
            .unwrap()
            .unwrap();
        assert_eq!(a.mode, m);
        assert!(!sc.what().is_empty());
        if c == Chain::CruiseWall {
            assert!(c.autopilot() == "cruise" && !c.lands());
            assert_eq!(reply(c, &[]).as_deref(), Some(DONE_CLAIM));
            assert_eq!(reply(c, &[DONE_CHECK]).as_deref(), Some(CLEAN));
            assert_eq!(
                reply(c, &[DONE_CHECK, &resume]).as_deref(),
                Some(DONE_CLAIM)
            );
        } else {
            assert!(c.autopilot() == "land" && c.lands(), "{c:?}");
            assert_eq!(reply(c, &[]).as_deref(), Some(WORK));
            assert_eq!(reply(c, &[LAND]).as_deref(), Some(CLEAN));
            assert_eq!(reply(c, &[LAND, &resume]).as_deref(), Some(CLEAN));
        }
    }
    let reach = |m: Mode| Harness::ALL.map(|h| wall_out_of_reach(h, m).is_none());
    assert_eq!(reach(Mode::WallDayStop), [false, true, false, false]);
    assert_eq!(reach(Mode::WallCruise), [true, true, true, true]);
    assert_eq!(reach(Mode::WallRelaunch), [false, true, true, true]);
    assert_eq!(reach(Mode::WallBucket), [false, true, false, false]);
    assert_eq!(
        Chain::of_mode_on(Mode::WallRelaunch, Harness::Claude),
        Vec::<Chain>::new()
    );
    assert_eq!(
        Chain::of_mode_on(Mode::WallBucket, Harness::Gemini),
        [Chain::BucketWall]
    );
    assert!(Harness::ALL.iter().all(|&h| Chain::DayStopWall.hides_on(h)
        && !Chain::CruiseWall.hides_on(h)
        && !Chain::RelaunchWall.hides_on(h)
        && !Chain::BucketWall.hides_on(h)));
    assert_eq!(
        Chain::DayStopWall.wall_claim_on(Harness::Gemini),
        WALL_CLAIM
    );
    assert_eq!(
        Chain::BucketWall.wall_claim_on(Harness::Gemini),
        crate::stub::gemini::BUCKET_CLAIM
    );
    assert_eq!(Chain::DayStopWall.wall_ahead_ms(), WALL_DAY_AHEAD_MS);
    assert_eq!(
        [Scenario::W7, Scenario::W8, Scenario::W9, Scenario::W10].map(Scenario::id),
        ["W7", "W8", "W9", "W10"]
    );
    // W9's relaunch: its wall stands (never lifted by the relaunch), lifted
    // at its reset as W1's; its window ends at the agent's exit
    let mut c = Cover {
        wall: Some((T0, 1_600)),
        ..Cover::default()
    };
    assert_eq!(wall_window_end(Chain::RelaunchWall, &c), Some(1_600_000));
    c.relaunch = Some(Switch {
        to: "muse",
        exited_ms: T0 + 150_000,
        ..Switch::default()
    });
    assert_eq!(wall_window_end(Chain::RelaunchWall, &c), Some(T0 + 150_000));
    assert_eq!(
        wall_move(Chain::RelaunchWall, &c, 1_600_000 - WALL_LIFT_EARLY_MS),
        WallMove::Lift
    );
}

/// The reason the loop raises its badge for Gemini CLI's usage-limit dialog
/// that names no reset (`approval.rs`'s `limit_no_reset_reason`).
const NO_RESET_BADGE: &str = "Gemini CLI's usage limit names no reset: Keep trying is tried on a \
     back-off ladder; /stats model says which limit";

/// The ladder's wait on Gemini CLI's per-day dialog.
const LADDER_WAIT: &str = "Gemini CLI's usage limit names no reset: Keep trying is pressed on the \
     back-off ladder";

/// W7's records on Gemini CLI (`--mode wall-day-stop`): the land act at +125
/// s met the per-day dialog (+140 s), the person told at once (`ESCALATED`,
/// `needs=ask`) beside the ladder's own wait for its first rung 30 min on
/// (+1940 s); the lab answered it `2. Stop` at +160 s; the Stop's error read
/// at the screen without the notice and waited on to the rung; the lab's wall
/// lifted at +25 min; the resume typed past the rung (+2010 s), NEEDS YOU
/// standing at every status read from the Stop to it, cleared after it, the
/// task landed.
fn gemini_day_stopped() -> Seen {
    let due = 140_000 + 1_800_000;
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            // the journal's order (measured, `z1gd`): the ladder's wait and
            // its badge are written before the point's own line
            j(
                140_000,
                &format!(
                    "WAITING seq=2 until={} rule=limit-retry@v1 {LADDER_WAIT}",
                    stamp(due)
                ),
            ),
            j(140_100, "ESCALATED seq=2 attention=OK"),
            j(
                140_100,
                "EVENT prompt seq=2 kind=usage-limit classify=- command=-",
            ),
            j(
                168_400,
                "EVENT limited seq=30 message=[API Error: An unknown error occurred.] reset=-",
            ),
            j(
                168_500,
                &format!(
                    "WAITING seq=30 until={} usage-session resets",
                    stamp(due + 60_000)
                ),
            ),
            j(
                due + 70_000,
                "CONTINUED seq=30 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(due + 74_000, "CLEARED seq=31 box attention=OK"),
            j(due + 75_000, "EVENT idle seq=31 Nothing left: the parser"),
            j(due + 75_000, "DONE seq=31 rule=land@v1 x"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            point(140_000, 2),
            row(
                140_100,
                "-",
                "escalated",
                "Keep trying (no reset named)",
                NO_RESET_BADGE,
            ),
            row(
                140_200,
                "limit-retry@v1",
                "waiting",
                "Keep trying (no reset named)",
                &format!("until …: {LADDER_WAIT}"),
            ),
            point(168_400, 2),
            typed(due + 70_000, "usage-resume@v1", LAND_NUDGE),
            point(due + 75_000, 3),
            task_done(due + 75_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status_of(141_000, "open", "ask", "land", "aterm-harness@1"),
            status(due + 76_000, "landed", "land"),
        ],
        status_reads: vec![T0 + 600_000, T0 + 1_200_000, T0 + 1_900_000],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + due + 75_000 + 545_000,
        stub: stub_read(),
        cover: Cover {
            wall: wall_at(1_500_000),
            lifted_ms: Some(T0 + 1_497_000),
            window_stub: window_read(),
            hidden: Some(Hidden {
                first_ms: T0 + 160_000,
                enter_ms: T0 + 160_000,
                seen_ms: T0 + 163_000,
                seq: Some(30),
                commands: 1,
                command: GEMINI_HIDE_OPTION,
                stopped: true,
                words: &GEMINI_WALL_WORDS,
            }),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// X1 PASSES W7: Gemini CLI's per-day dialog answered `Stop` is KEPT TRYING —
/// the resume typed at the ladder's first rung from the Stop's error, NEEDS
/// YOU standing at every status read from the Stop to it, taken down after
/// it got through, landed. NEGATIVE CONTROLS: the Stop's error handed to a
/// person and nothing typed (the session blocked, as before X1), a resume
/// before the rung, another act first, NEEDS YOU down while the limit stood
/// (a `CLEARED`, a status read of `needs=-`), and NEEDS YOU standing past the
/// resume that got through each FAIL; the notice taken by a local command
/// rather than the Stop, or no status read between the Stop and the resume,
/// did not drive it: UNOBSERVED.
#[test]
fn the_per_day_dialog_answered_stop_is_kept_trying_while_needs_you_stands() {
    let w7 = |s: &Seen| one(Chain::DayStopWall, s, "gemini", Scenario::W7);
    let (w, why) = w7(&gemini_day_stopped());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains(
            "kept trying: usage-resume@v1 at +2010.0 s (70.0 s after the ladder's first due)"
        ) && why.contains("needs=ask at 3 status read(s) from the Stop to it")
            && why.contains("NEEDS YOU down at +2014.0 s"),
        "{why}"
    );
    let fail = |s: &Seen, needle: &str| {
        let (w, why) = w7(s);
        assert_eq!(w, "FAIL", "{needle}: {why}");
        assert!(why.contains(needle), "{needle}: {why}");
    };
    let due = 140_000 + 1_800_000;
    // the Stop's error handed to a person, nothing typed: blocked
    let mut s = gemini_day_stopped();
    s.journal.truncate(8);
    s.ledger.truncate(6);
    s.ledger.push(row(
        168_450,
        "-",
        "escalated",
        "",
        "api-error not retried (Gemini CLI retries what time answers; after its usage limit's Stop, see /stats model): [API Error: An unknown error occurred.]",
    ));
    s.status.truncate(2);
    fail(&s, "ESCALATED");
    // the resume before the ladder's rung (past the lab's lift)
    let mut s = gemini_day_stopped();
    s.journal[8].0 = T0 + due - 100_000;
    s.ledger[6].ts = T0 + due - 100_000;
    fail(&s, "RESUMED BEFORE ITS RUNG: usage-resume@v1 at +1840.0 s");
    // another act first
    let mut s = gemini_day_stopped();
    s.journal[8].1 = "CONTINUED seq=30 rule=land@v1 [aterm harness] Autopilot is Land:".into();
    s.ledger[6] = typed(due + 70_000, "land@v1", LAND);
    let (w, why) = w7(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("the first act after the Stop was land@v1"),
        "{why}"
    );
    // NEEDS YOU down while the limit stood
    let mut s = gemini_day_stopped();
    s.journal
        .insert(8, j(600_000, "CLEARED seq=30 box attention=OK"));
    fail(&s, "NEEDS YOU came down at +600.0 s while the limit stood");
    let mut s = gemini_day_stopped();
    s.status.insert(2, status(900_000, "open", "land"));
    fail(&s, "NEEDS YOU not standing at the status read at +900.0 s");
    // NEEDS YOU standing past the resume that got through
    let mut s = gemini_day_stopped();
    s.journal.remove(9);
    s.status[2] = status_of(due + 76_000, "landed", "ask", "land", "aterm-harness@1");
    fail(&s, "NEEDS YOU STOOD after the resume that got through");
    // the notice taken by a local command, not the dialog's Stop (W2's
    // judge passing on the screen it left)
    let mut s = gemini_day_stopped();
    let h = s.cover.hidden.as_mut().expect("hidden");
    h.stopped = false;
    h.command = HIDE_COMMAND;
    s.journal[6].1 = "EVENT idle seq=30 -".into();
    let (w, why) = w7(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("not the dialog's Stop"), "{why}");
    // no status read between the Stop and the resume
    let mut s = gemini_day_stopped();
    s.status_reads.clear();
    let (w, why) = w7(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("no status read between the Stop and the resume"),
        "{why}"
    );
}

/// W8's records on Muse Code (`--mode wall-cruise`): the done claim's point
/// (+5 s), Cruise's done check at +125 s met the wall (reset +300 s), read
/// and waited on; the resume typed past it (+361 s) answered with the done
/// claim again, and Cruise's own done check a back-off after it (+605 s).
fn muse_cruise_walled() -> Seen {
    Seen {
        journal: vec![
            j(5_000, &format!("EVENT idle seq=1 {DONE_CLAIM}")),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=continue-done-check@v1 [aterm harness] done is never the end",
            ),
            j(140_000, MUSE_LIMITED),
            j(
                140_100,
                &format!(
                    "WAITING seq=2 until={} usage-session resets",
                    stamp(300_000)
                ),
            ),
            j(
                361_000,
                "CONTINUED seq=2 rule=usage-resume@v1 [aterm harness] carry on",
            ),
            j(365_000, &format!("EVENT idle seq=3 {DONE_CLAIM}")),
            j(
                365_100,
                "WAITING seq=3 until=x 2 short turn(s) in a row: 4 min back-off",
            ),
            j(
                605_000,
                "CONTINUED seq=3 rule=continue-done-check@v1 [aterm harness] done is never the end",
            ),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "continue-done-check@v1", DONE_CHECK),
            point(140_000, 2),
            typed(361_000, "usage-resume@v1", LAND_NUDGE),
            point(365_000, 2),
            typed(605_000, "continue-done-check@v1", DONE_CHECK),
        ],
        status: vec![status(1_000, "open", "cruise")],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 615_000,
        cover: Cover {
            wall: wall_at(300_000),
            lifted_ms: Some(T0 + 297_000),
            window_stub: window_read(),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// A USAGE WALL UNDER CRUISE PASSES W8: Cruise's act met the wall, nothing
/// typed into it, the resume past it, and Cruise's own act after the
/// resume's answer; on Claude Code its own continuation past the reset is
/// the going on, and Cruise's act after it is no act on top of it. The watch
/// ends at Cruise's own act ([`walls::cruise_went_on`]), never at the resume.
/// NEGATIVE CONTROLS: an act typed into the wall, a DONE under Cruise,
/// Cruise's act past the reset with no resume before it, a resume past its
/// deadline, the resume typed on top of the program's own continuation, and
/// no act of Cruise's own after the resume each FAIL; a wall never drawn:
/// UNOBSERVED.
#[test]
fn a_usage_wall_under_cruise_is_waited_out_and_cruise_goes_on() {
    let w8 = |s: &Seen, p: &str| one(Chain::CruiseWall, s, p, Scenario::W8);
    let (w, why) = w8(&muse_cruise_walled(), "muse");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("Cruise's continue-done-check@v1 typed at +125.0 s met it")
            && why.contains("gone on: usage-resume@v1 at +361.0 s (61.0 s past the reset)")
            && why.contains("Cruise went on: continue-done-check@v1 at +605.0 s"),
        "{why}"
    );
    assert_eq!(
        crate::forever::walls::cruise_went_on(&muse_cruise_walled()),
        Some(T0 + 605_000)
    );
    assert_eq!(
        crate::forever::walls::cruise_met(&muse_cruise_walled()),
        Some(T0 + 125_000)
    );
    let fail = |s: &Seen, p: &str, needle: &str| {
        let (w, why) = w8(s, p);
        assert_eq!(w, "FAIL", "{needle}: {why}");
        assert!(why.contains(needle), "{needle}: {why}");
    };
    // typed into the wall
    let mut s = muse_cruise_walled();
    s.journal.insert(
        5,
        j(
            250_000,
            "CONTINUED seq=2 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going",
        ),
    );
    s.ledger
        .insert(3, typed(250_000, "continue@v1", CONTINUE_TEXT));
    fail(&s, "muse", "TYPED into the wall: continue@v1 at +250.0 s");
    // DONE under Cruise
    let mut s = muse_cruise_walled();
    s.journal.push(j(606_000, "DONE seq=4 rule=land@v1 x"));
    fail(&s, "muse", "DONE under Cruise at +606.0 s");
    // Cruise's act past the reset with no resume before it
    let mut s = muse_cruise_walled();
    s.journal[5].1 =
        "CONTINUED seq=2 rule=continue-done-check@v1 [aterm harness] done is never the end".into();
    s.ledger[3] = typed(361_000, "continue-done-check@v1", DONE_CHECK);
    fail(&s, "muse", "NO RESUME");
    // the resume past its deadline (the minute, 60 s and 5 min)
    let mut s = muse_cruise_walled();
    s.journal[5].0 = T0 + 800_000;
    s.ledger[3].ts = T0 + 800_000;
    s.journal.truncate(6);
    s.ledger.truncate(4);
    s.end_ms = T0 + 900_000;
    fail(&s, "muse", "PERSISTENCE LOST: usage-resume@v1 at +800.0 s");
    // no act of Cruise's own after the resume
    let mut s = muse_cruise_walled();
    s.journal.truncate(8);
    s.ledger.truncate(5);
    s.end_ms = T0 + 361_000 + 400_000;
    assert_eq!(crate::forever::walls::cruise_went_on(&s), None);
    fail(
        &s,
        "muse",
        "PERSISTENCE LOST: Cruise typed nothing of its own",
    );
    // Claude Code going on by itself, Cruise's check after its answer
    let mut s = muse_cruise_walled();
    s.journal.remove(5);
    s.ledger.remove(3);
    s.cover.went_on_ms = Some(T0 + 340_000);
    let (w, why) = w8(&s, "claude");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("gone on: the program's own continuation at +340.0 s")
            && why.contains("Cruise went on: continue-done-check@v1 at +605.0 s"),
        "{why}"
    );
    // ... and the resume typed on top of it
    let mut s = muse_cruise_walled();
    s.cover.went_on_ms = Some(T0 + 340_000);
    fail(
        &s,
        "claude",
        "TYPED ON TOP of the program's own continuation",
    );
    // the wall never drawn as a limit point
    let mut s = muse_cruise_walled();
    s.journal.remove(3);
    let (w, why) = w8(&s, "muse");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never drawn as a limit point"), "{why}");
    // CRUISE'S ACT IS DUE WHERE THE LOOP SAID: the wall's points climbed the
    // streak a step further (measured, `z4cr-claude`: 3 short turns, 8 min),
    // and Cruise's act at that back-off's due passes
    let climbed = |act_at: i64| {
        let mut s = muse_cruise_walled();
        s.journal[7] = j(
            365_100,
            "WAITING seq=3 until=x 3 short turn(s) in a row: 8 min back-off",
        );
        s.journal[8].0 = T0 + act_at;
        s.ledger[5].ts = T0 + act_at;
        s.end_ms = T0 + act_at + 10_000;
        s
    };
    let (w, why) = w8(&climbed(845_000), "muse");
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains(
            "the back-off it named due +845.0 s (3 short turn(s) in a row: 8 min back-off)"
        ),
        "{why}"
    );
    // NEGATIVE CONTROLS: the act past that due and its grace; and, with no
    // back-off named, the act past the fixed window
    fail(
        &climbed(845_000 + 61_000),
        "muse",
        "PERSISTENCE LOST: Cruise's continue-done-check@v1 only at +906.0 s",
    );
    let mut s = climbed(845_000);
    s.journal.remove(7);
    fail(
        &s,
        "muse",
        "PERSISTENCE LOST: Cruise's continue-done-check@v1 only at +845.0 s",
    );
}

/// W9's records on Muse Code (`--mode wall-relaunch`): the land act at +125 s
/// met the wall (reset +600 s), read and waited on; the lab ended Muse Code at
/// +150 s and launched it again at +153 s, its prompt typed at +160 s meeting
/// the wall again (+170 s), waited on to the reset; the resume past it
/// (+661 s) landed at +665 s.
fn muse_relaunched() -> Seen {
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(140_000, MUSE_LIMITED),
            j(
                140_100,
                &format!(
                    "WAITING seq=2 until={} usage-session resets",
                    stamp(600_000)
                ),
            ),
            j(170_000, &MUSE_LIMITED.replace("seq=2", "seq=8")),
            j(
                170_100,
                &format!(
                    "WAITING seq=8 until={} usage-session resets",
                    stamp(600_000)
                ),
            ),
            j(
                661_000,
                "CONTINUED seq=8 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(665_000, "EVENT idle seq=9 Nothing left: the parser"),
            j(665_000, "DONE seq=9 rule=land@v1 x"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            point(140_000, 2),
            point(170_000, 2),
            typed(661_000, "usage-resume@v1", LAND_NUDGE),
            point(665_000, 3),
            task_done(665_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(666_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 665_000 + 1_025_000,
        stub: stub_read(),
        cover: Cover {
            wall: wall_at(600_000),
            lifted_ms: Some(T0 + 597_000),
            window_stub: window_read(),
            relaunch: Some(Switch {
                to: "muse",
                exited_ms: T0 + 150_000,
                shell_ms: T0 + 152_000,
                lifted_ms: None,
                launched_ms: T0 + 153_000,
                prompt_ms: T0 + 160_000,
            }),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// THE RELAUNCH UNDER A STANDING WALL PASSES W9: nothing typed into the wall
/// before or after the relaunch, the wall read again on the relaunched
/// agent's prompt and waited out, landed; one resume typed at once after
/// the relaunch, before the wall is read again (the try a seat nothing
/// vouches for gets), passes too, said. NEGATIVE CONTROLS: an act into the
/// wall before the relaunch, a second try, an act after the wall was read
/// again, a resume past its deadline, and a landing while the wall stood
/// each FAIL; no relaunch made, or a wall never read after it: UNOBSERVED.
#[test]
fn an_agent_launched_again_under_its_wall_waits_it_out() {
    let w9 = |s: &Seen| one(Chain::RelaunchWall, s, "muse", Scenario::W9);
    let (w, why) = w9(&muse_relaunched());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("launched again at +153.0 s and prompted at +160.0 s")
            && why.contains("the wall read again at +170.0 s")
            && why.contains("gone on: usage-resume@v1 at +661.0 s (61.0 s past the reset)"),
        "{why}"
    );
    // the try typed at the relaunched agent's banner, its point wordless:
    // the wait before the relaunch named no due of it
    let probe = |s: &mut Seen, t: i64| {
        s.journal.insert(
            5,
            j(
                t,
                "CONTINUED seq=7 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
        );
        s.ledger.insert(3, typed(t, "usage-resume@v1", LAND_NUDGE));
    };
    // the one try at once
    let mut s = muse_relaunched();
    probe(&mut s, 158_000);
    let (w, why) = w9(&s);
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("one try at once after the relaunch: usage-resume@v1 at +158.0 s"),
        "{why}"
    );
    let fail = |s: &Seen, needle: &str| {
        let (w, why) = w9(s);
        assert_eq!(w, "FAIL", "{needle}: {why}");
        assert!(why.contains(needle), "{needle}: {why}");
    };
    // NEGATIVE CONTROL of the records' own check: the same try with no
    // relaunch between it and the wait is typed before that wait's due
    let mut s = muse_relaunched();
    probe(&mut s, 158_000);
    s.cover.relaunch.as_mut().expect("relaunch").exited_ms = T0 + 159_000;
    fail(
        &s,
        "usage-resume@v1 typed 442.0 s BEFORE the due its back-off named",
    );
    // a second try
    let mut s = muse_relaunched();
    probe(&mut s, 158_000);
    probe(&mut s, 165_000);
    s.journal.swap(5, 6);
    s.ledger.swap(3, 4);
    fail(
        &s,
        "TYPED into the wall after the relaunch: usage-resume@v1 at +165.0 s",
    );
    // a try after the wall was read again
    let mut s = muse_relaunched();
    s.journal.insert(
        7,
        j(
            300_000,
            "CONTINUED seq=8 rule=usage-resume@v1 [aterm harness] Still landing:",
        ),
    );
    s.ledger
        .insert(4, typed(300_000, "usage-resume@v1", LAND_NUDGE));
    fail(
        &s,
        "TYPED into the wall after the relaunch: usage-resume@v1 at +300.0 s",
    );
    // typed into the wall before the relaunch
    let mut s = muse_relaunched();
    s.journal.insert(
        5,
        j(
            145_000,
            "CONTINUED seq=2 rule=usage-resume@v1 [aterm harness] Still landing:",
        ),
    );
    s.ledger
        .insert(3, typed(145_000, "usage-resume@v1", LAND_NUDGE));
    fail(
        &s,
        "TYPED into the wall before the relaunch: usage-resume@v1 at +145.0 s",
    );
    // the resume past its deadline (the minute, 60 s and 5 min)
    let mut s = muse_relaunched();
    for (t, _) in s.journal.iter_mut().skip(7) {
        *t += 400_000;
    }
    for r in s.ledger.iter_mut().skip(4) {
        r.ts += 400_000;
    }
    s.status[1].0 += 400_000;
    s.end_ms += 400_000;
    fail(&s, "PERSISTENCE LOST: usage-resume@v1 at +1061.0 s");
    // landed while the wall stood
    let mut s = muse_relaunched();
    s.journal.insert(7, j(400_000, "DONE seq=8 rule=land@v1 x"));
    fail(&s, "LANDED at +400.0 s while the wall stood");
    // no relaunch made
    let mut s = muse_relaunched();
    s.cover.relaunch = None;
    s.cover.relaunch_gave_up = Some("muse never stood at an empty composer within 90 s".into());
    let (w, why) = w9(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("no relaunch made (muse never stood"), "{why}");
    // the wall never read after the relaunch
    let mut s = muse_relaunched();
    s.journal.remove(5);
    let (w, why) = w9(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("never read the wall before the reset"),
        "{why}"
    );
}

/// W10's records on Gemini CLI (`--mode wall-bucket`): the land act at +125 s
/// met the bucket's dialog (+140 s, a reset named), waited on to the reset
/// (`Stop is pressed and the turn resumed past it`); its `Stop` at the due
/// (+361 s), the error it left read, the resume typed there (+363 s),
/// landed.
fn gemini_bucket() -> Seen {
    let waits = "Gemini CLI's usage limit resets at 5:06 AM: Stop is pressed and the turn resumed \
         past it";
    Seen {
        journal: vec![
            j(5_000, "EVENT idle seq=1 Implemented the parser"),
            j(
                5_100,
                "WAITING seq=1 until=x 1 short turn(s) in a row: 2 min back-off",
            ),
            j(
                125_000,
                "CONTINUED seq=1 rule=land@v1 [aterm harness] Autopilot is Land:",
            ),
            j(
                140_000,
                "EVENT prompt seq=2 kind=usage-limit classify=- command=-",
            ),
            j(
                140_100,
                &format!(
                    "WAITING seq=2 until={} rule=limit-retry@v1 {waits}",
                    stamp(360_000)
                ),
            ),
            j(361_000, "APPROVED seq=2 Stop (reset 5:06 AM)"),
            j(
                362_000,
                "EVENT limited seq=3 message=[API Error: An unknown error occurred.] reset=-",
            ),
            j(
                363_000,
                "CONTINUED seq=3 rule=usage-resume@v1 [aterm harness] Still landing:",
            ),
            j(367_000, "EVENT idle seq=4 Nothing left: the parser"),
            j(367_000, "DONE seq=4 rule=land@v1 x"),
        ],
        ledger: vec![
            point(5_000, 1),
            typed(125_000, "land@v1", LAND),
            point(140_000, 2),
            row(
                140_100,
                "limit-retry@v1",
                "waiting",
                "Stop (reset 5:06 AM)",
                &format!("until …: {waits}"),
            ),
            row(
                361_000,
                "limit-stop@v1",
                "approved",
                "Stop (reset 5:06 AM)",
                "",
            ),
            point(362_000, 2),
            typed(363_000, "usage-resume@v1", LAND_NUDGE),
            point(367_000, 3),
            task_done(367_000),
        ],
        status: vec![
            status(1_000, "open", "land"),
            status(368_000, "landed", "land"),
        ],
        meta: vec![no_meta(1_000)],
        task: vec![j(1_000, "OK task -")],
        end_ms: T0 + 367_000 + 545_000,
        stub: stub_read(),
        cover: Cover {
            wall: wall_at(300_000),
            lifted_ms: Some(T0 + 297_000),
            window_stub: window_read(),
            ..Cover::default()
        },
        ..Seen::default()
    }
}

/// THE PER-MODEL BUCKET PASSES W10: its dialog's `Stop` at the due its wait
/// named, the resume typed at the error it left, never the switch, landed.
/// NEGATIVE CONTROLS: the switch chosen, a `Stop` past the reset but before
/// its due, a `Stop` in the wall, and the resume typed before the `Stop` each
/// FAIL; the account's dialog (its `Keep trying` pressed, no `Stop`): the
/// bucket was not drawn, UNOBSERVED.
#[test]
fn the_per_model_buckets_dialog_gets_its_stop_never_its_switch() {
    let w10 = |s: &Seen| one(Chain::BucketWall, s, "gemini", Scenario::W10);
    let (w, why) = w10(&gemini_bucket());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("the bucket's Stop at +361.0 s, usage-resume@v1 at +363.0 s")
            && why.contains("never the switch"),
        "{why}"
    );
    let fail = |s: &Seen, needle: &str| {
        let (w, why) = w10(s);
        assert_eq!(w, "FAIL", "{needle}: {why}");
        assert!(why.contains(needle), "{needle}: {why}");
    };
    let mut s = gemini_bucket();
    s.ledger[4].command = "Switch to gemini-2.5-flash".into();
    fail(&s, "SWITCHED the model at +361.0 s");
    let mut s = gemini_bucket();
    s.ledger[4].ts = T0 + 340_000;
    fail(&s, "STOPPED BEFORE ITS DUE");
    let mut s = gemini_bucket();
    s.ledger[4].ts = T0 + 200_000;
    fail(&s, "TYPED into the wall");
    let mut s = gemini_bucket();
    s.journal[7].0 = T0 + 350_000;
    s.ledger[6].ts = T0 + 350_000;
    fail(&s, "NO RESUME typed after the Stop");
    // the account's dialog: its Keep trying pressed past the reset
    let mut s = gemini_bucket();
    s.ledger[4] = approved(361_000);
    s.journal.remove(7);
    s.ledger.remove(6);
    let (w, why) = w10(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("the dialog drawn was not the bucket's"),
        "{why}"
    );
}
