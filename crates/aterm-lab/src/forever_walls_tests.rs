// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the wall modes past W2 (`--mode wall-redraw` through `wall-relaunch`).

use super::*;

const MUSE_LIMITED: &str = "EVENT limited seq=2 message=◆ Usage limit reached · /upgrade (x) for increased limits, or wait for usage to reset at 1:06 PM reset=1:06 PM";

fn wall_at(reset: i64) -> Option<(i64, i64)> {
    Some((T0 + 6_000, (T0 + reset) / 1000))
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

#[test]
fn the_wall_is_raised_again_at_its_reset_as_each_chain_shifts() {
    let reset = 1_300;
    let mut c = Cover {
        wall: Some((T0, reset)),
        ..Cover::default()
    };
    let due = reset * 1000 - WALL_LIFT_EARLY_MS;
    assert_eq!(wall_move(Chain::RedrawWall, &c, due - 1), WallMove::Hold);
    let step = REDRAW_AHEAD_MS / 1000;
    assert_eq!(
        wall_move(Chain::RedrawWall, &c, due),
        WallMove::Shift(reset + step)
    );
    for chain in [Chain::Wall, Chain::WallHid, Chain::UnreadWall] {
        assert_eq!(wall_move(chain, &c, due), WallMove::Lift, "{chain:?}");
    }
    assert_eq!(
        wall_move(Chain::SwitchWall, &c, due + 3_600_000),
        WallMove::Hold
    );
    c.shifted.push((due, reset + step));
    assert_eq!(
        wall_move(Chain::RedrawWall, &c, due + 10_000),
        WallMove::Hold
    );
    let due2 = (reset + step) * 1000 - WALL_LIFT_EARLY_MS;
    assert_eq!(wall_move(Chain::RedrawWall, &c, due2), WallMove::Lift);
    c.lifted_ms = Some(due2);
    assert_eq!(wall_move(Chain::RedrawWall, &c, due2 + 1), WallMove::Hold);
    assert_eq!(
        wall_move(Chain::RedrawWall, &Cover::default(), due),
        WallMove::Hold
    );
    assert_eq!(REDRAW_AHEAD_MS % 60_000, 0);
    assert_eq!(
        [Chain::RedrawWall, Chain::Wall, Chain::SwitchWall].map(Chain::shifts),
        [1, 0, 0]
    );
    assert_eq!(
        [Chain::RedrawWall, Chain::Wall].map(Chain::shift_step_ms),
        [REDRAW_AHEAD_MS, 0]
    );
}

#[test]
fn a_requests_reset_is_the_wall_standing_when_it_was_answered() {
    let mut c = Cover {
        wall: Some((T0, 1_300)),
        shifted: vec![(T0 + 297_000, 1_600)],
        lifted_ms: Some(T0 + 597_000),
        ..Cover::default()
    };
    assert_eq!(c.reset_now(), Some(1_600));
    assert_eq!(Cover::default().reset_now(), None);
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

#[test]
fn the_switch_starts_another_agent_once_the_first_has_left_its_tab() {
    for h in Harness::ALL {
        assert_ne!(switch_to(h), h, "{h:?}");
    }
    assert_eq!(
        Harness::ALL.map(switch_to),
        [Harness::Muse, Harness::Muse, Harness::Claude]
    );
    assert_eq!(
        Harness::ALL.map(exit_of),
        [
            Exit::CancelThenCtrlCTwice(CLAUDE_CANCEL),
            Exit::Command("/quit"),
            Exit::CtrlCTwice
        ]
    );
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
    assert_eq!(switch_watch_until(1_000), 1_000_000 + 150_000 + 60_000);
}

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

#[test]
fn the_walls_past_w2_each_run_their_own_chain_where_they_reach() {
    let modes = [
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
        let a = parse_args(&v(&["--harness", "agy", "--mode", w]))
            .unwrap()
            .unwrap();
        assert_eq!(a.mode, m);
        assert_eq!(reply(c, &[]).as_deref(), Some(WORK));
        assert_eq!(reply(c, &[LAND]).as_deref(), Some(CLEAN));
        let resume = format!("{MARK} carry on");
        assert_eq!(reply(c, &[LAND, &resume]).as_deref(), Some(CLEAN));
        assert!(!sc.what().is_empty());
    }
    let reach = |m: Mode| Harness::ALL.map(|h| wall_out_of_reach(h, m).is_none());
    assert_eq!(reach(Mode::WallRedraw), [true, true, true]);
    assert_eq!(reach(Mode::WallSwitch), [true, true, true]);
    assert_eq!(reach(Mode::WallUnread), [true, false, false]);
    assert_eq!(
        Chain::of_mode_on(Mode::WallUnread, Harness::Muse),
        Vec::<Chain>::new()
    );
    assert_eq!(
        Chain::of_mode_on(Mode::WallUnread, Harness::Claude),
        [Chain::UnreadWall]
    );
    assert_eq!(
        Harness::ALL.map(|h| Chain::RedrawWall.hides_on(h)),
        [true, false, false]
    );
    assert!(Harness::ALL.iter().all(|&h| Chain::UnreadWall.hides_on(h)
        && Chain::WallHid.hides_on(h)
        && !Chain::SwitchWall.hides_on(h)
        && !Chain::Wall.hides_on(h)));
    assert_eq!(
        [Scenario::W4, Scenario::W5, Scenario::W6].map(Scenario::id),
        ["W4", "W5", "W6"]
    );
    assert_eq!(
        unread_told_words(),
        aterm_agent::supervise::policy::exception::Exception::unread_limit().why
    );
}

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
    let mut s = muse_redrawn();
    s.journal[5].0 = T0 + 250_000;
    s.ledger[3].ts = T0 + 250_000;
    fail(
        &s,
        "muse",
        "TYPED into the wall: usage-resume@v1 at +250.0 s",
    );
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
    let mut s = muse_redrawn();
    s.journal.insert(8, j(500_000, "DONE seq=3 rule=land@v1 x"));
    fail(&s, "muse", "LANDED at +500.0 s while the wall stood");
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
    let mut s = muse_redrawn();
    s.cover.went_on_ms = Some(T0 + 640_000);
    fail(
        &s,
        "claude",
        "TYPED ON TOP of the program's own continuation",
    );
    let mut s = muse_redrawn();
    s.cover.shifted.clear();
    let (w, why) = w4(&s, "muse");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never raised again"), "{why}");
    let mut s = muse_redrawn();
    s.journal.remove(6);
    let (w, why) = w4(&s, "muse");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("the redraw was not seen"), "{why}");
}

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

#[test]
fn the_redraw_after_a_typed_resume_is_waited_on_the_loops_floor() {
    let w4 = |s: &Seen| one(Chain::RedrawWall, s, "agy", Scenario::W4);
    let (w, why) = w4(&agy_redrawn());
    assert_eq!(w, "PASS", "{why}");
    assert!(
        why.contains("gone on: usage-resume@v1 at +1185.0 s (421.0 s past the new reset; the loop's floor after a resume that met the wall again: +1184.3 s)"),
        "{why}"
    );
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
    let mut s = agy_redrawn();
    s.journal[6].1 = "CONTINUED seq=2 rule=land@v1 [aterm harness] Autopilot is Land:".into();
    s.ledger[3] = typed(553_800, "land@v1", LAND);
    let (w, why) = w4(&s);
    assert_eq!(w, "FAIL", "{why}");
    assert!(
        why.contains("PERSISTENCE LOST: usage-resume@v1 at +1185.0 s, 421.0 s past the new reset"),
        "{why}"
    );
}

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
    let mut s = claude_switched();
    s.journal.truncate(7);
    s.ledger.truncate(4);
    fail(&s, "claude's LIMIT HELD muse: no land act");
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
    let mut s = claude_switched();
    s.journal.push(j(
        1_175_000,
        "CONTINUED seq=12 rule=usage-resume@v1 [aterm harness] Still landing:",
    ));
    s.ledger
        .push(typed(1_175_000, "usage-resume@v1", LAND_NUDGE));
    fail(&s, "TYPED after the landing");
    let mut s = claude_switched();
    s.journal.insert(
        5,
        j(
            160_000,
            "CONTINUED seq=2 rule=usage-resume@v1 [aterm harness] Still landing:",
        ),
    );
    fail(&s, "TYPED into claude's wall: usage-resume@v1 at +160.0 s");
    let mut s = claude_switched();
    s.cover.switch = None;
    s.cover.switch_gave_up = Some("no muse in the lab (LAB_MUSE at up)".into());
    let (w, why) = w5(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("no switch made (no muse in the lab"), "{why}");
    let mut s = claude_switched();
    s.end_ms = T0 + 700_000;
    let (w, why) = w5(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("short of claude's reset"), "{why}");
}

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
    let mut s = muse_nudged();
    s.journal[7] = j(
        470_500,
        "CONTINUED seq=10 rule=continue@v1 [aterm harness] Keep going",
    );
    s.ledger[4] = typed(470_500, "continue@v1", "Keep going");
    fail(&s, "TYPED before muse's land act: continue@v1 at +470.5 s");
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
    s.end_ms = T0 + 531_000;
    fail(
        &s,
        "claude's LIMIT HELD muse: no land act in the 311.0 s after its prompt (due by +530.0 s",
    );
}

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
    let mut s = claude_held_unread();
    s.journal.remove(7);
    fail(&s, "NOT TOLD");
    let mut s = claude_held_unread();
    s.journal[7].0 = T0 + 3_000_000;
    fail(&s, "TOLD EARLY at +3000.0 s");
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
    let mut s = claude_held_unread();
    s.journal.insert(
        8,
        j(4_050_000, "CLEARED seq=40 exception=futile after=30s x"),
    );
    fail(
        &s,
        "the told came down at +4050.0 s while the limit was still held",
    );
    let mut s = claude_held_unread();
    s.journal.remove(8);
    fail(&s, "the told STOOD past the landing");
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
    let mut s = claude_held_unread();
    s.journal.remove(6);
    let (w, why) = w6(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("the loop never held the limit unread"),
        "{why}"
    );
    let mut s = claude_held_unread();
    s.cover.unread = None;
    s.cover.unread_gave_up = Some("no transcript holds FOREVERLAB-UNREAD-WALL.".into());
    let (w, why) = w6(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("no transcript holds"), "{why}");
}

#[test]
fn the_walls_of_x1_and_x2_each_run_their_own_chain_where_they_reach() {
    let modes = [
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
        let a = parse_args(&v(&["--harness", "agy", "--mode", w]))
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
    assert_eq!(reach(Mode::WallCruise), [true, true, true]);
    assert_eq!(reach(Mode::WallRelaunch), [false, true, true]);
    assert_eq!(
        Chain::of_mode_on(Mode::WallRelaunch, Harness::Claude),
        Vec::<Chain>::new()
    );
    assert_eq!(
        Chain::of_mode_on(Mode::WallRelaunch, Harness::Agy),
        [Chain::RelaunchWall]
    );
    assert!(
        Harness::ALL
            .iter()
            .all(|&h| !Chain::CruiseWall.hides_on(h) && !Chain::RelaunchWall.hides_on(h))
    );
    assert_eq!([Scenario::W8, Scenario::W9].map(Scenario::id), ["W8", "W9"]);
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
    let mut s = muse_cruise_walled();
    s.journal.push(j(606_000, "DONE seq=4 rule=land@v1 x"));
    fail(&s, "muse", "DONE under Cruise at +606.0 s");
    let mut s = muse_cruise_walled();
    s.journal[5].1 =
        "CONTINUED seq=2 rule=continue-done-check@v1 [aterm harness] done is never the end".into();
    s.ledger[3] = typed(361_000, "continue-done-check@v1", DONE_CHECK);
    fail(&s, "muse", "NO RESUME");
    let mut s = muse_cruise_walled();
    s.journal[5].0 = T0 + 800_000;
    s.ledger[3].ts = T0 + 800_000;
    s.journal.truncate(6);
    s.ledger.truncate(4);
    s.end_ms = T0 + 900_000;
    fail(&s, "muse", "PERSISTENCE LOST: usage-resume@v1 at +800.0 s");
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
    let mut s = muse_cruise_walled();
    s.cover.went_on_ms = Some(T0 + 340_000);
    fail(
        &s,
        "claude",
        "TYPED ON TOP of the program's own continuation",
    );
    let mut s = muse_cruise_walled();
    s.journal.remove(3);
    let (w, why) = w8(&s, "muse");
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("never drawn as a limit point"), "{why}");
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
    let mut s = muse_relaunched();
    probe(&mut s, 158_000);
    s.cover.relaunch.as_mut().expect("relaunch").exited_ms = T0 + 159_000;
    fail(
        &s,
        "usage-resume@v1 typed 442.0 s BEFORE the due its back-off named",
    );
    let mut s = muse_relaunched();
    probe(&mut s, 158_000);
    probe(&mut s, 165_000);
    s.journal.swap(5, 6);
    s.ledger.swap(3, 4);
    fail(
        &s,
        "TYPED into the wall after the relaunch: usage-resume@v1 at +165.0 s",
    );
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
    let mut s = muse_relaunched();
    s.journal.insert(7, j(400_000, "DONE seq=8 rule=land@v1 x"));
    fail(&s, "LANDED at +400.0 s while the wall stood");
    let mut s = muse_relaunched();
    s.cover.relaunch = None;
    s.cover.relaunch_gave_up = Some("muse never stood at an empty composer within 90 s".into());
    let (w, why) = w9(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(why.contains("no relaunch made (muse never stood"), "{why}");
    let mut s = muse_relaunched();
    s.journal.remove(5);
    let (w, why) = w9(&s);
    assert_eq!(w, "UNOBSERVED", "{why}");
    assert!(
        why.contains("never read the wall before the reset"),
        "{why}"
    );
}
