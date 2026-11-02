// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The loop over a point that is the turn the program drew, never its composer
//! (ATERM-KI-1101-2): a draft a person types and clears is no new point, a due that falls on a
//! frame drawn for one read is decided at the next look, and at a limit wall a point moves with the
//! model agy's footer names, and with no more of its footer.

use super::*;
use crate::supervise::run::tests::point_key::{agy_drafted, agy_footer_left};
use aterm_phase::prompt::fixtures::screen;

/// agy's footer as its first ctrl+c draws it ([`aterm_phase::agy::fixtures::EXIT_CONFIRM`]): no
/// idle evidence.
const EXIT_HINT: &str = aterm_phase::anchor!("agy.footer.exit");

/// The journal's points (`EVENT idle`, and `EVENT limited` at a limit wall) a loop wrote over
/// `screens` under `program`'s mock, a person's key 2 s old at every read, so the person's grace
/// holds every act.
fn points_under_a_person(tag: &str, program: &str, screens: Vec<Vec<String>>) -> Vec<String> {
    let mut m = match program {
        "agy" => agy_mock(screens),
        "muse" => muse_mock(screens),
        _ => fenced(screens),
    };
    m.human = vec!["2000"];
    m.vanish_after = Some(4);
    let (dir, path) = journal_file(tag);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(600)
    };
    let _ = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        count(&m, "send") + count(&m, "turn"),
        0,
        "{tag}: the person's grace holds every act: {:#?}",
        m.requests
    );
    records
        .into_iter()
        .map(|r| r.line)
        .filter(|l| l.starts_with("EVENT idle seq=") || l.starts_with("EVENT limited seq="))
        .collect()
}

/// A DRAFT A PERSON TYPES AND CLEARS AT A TURN END IS NO NEW POINT, under agy as under Claude Code
/// and Muse Code: the journal names the turn's end once. NEGATIVE CONTROL: a second turn ended
/// after the same draft and clearing is a second point.
#[test]
fn a_draft_typed_and_cleared_at_a_turn_end_is_no_new_point() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::muse::fixtures as u;
    let caret_typed = |rows: &[String], caret: usize| {
        let mut out = rows.to_vec();
        out[caret] = "❯ a note for later".to_string();
        out
    };
    let agy_end = screen(a::END_OF_TURN);
    let mut agy_next = agy_end.clone();
    let last = agy_next
        .iter()
        .rposition(|r| r.starts_with("  and ocean floor depth"))
        .expect("the reply's last row");
    agy_next[last] = "  and ocean floor depth shape these rhythms further.".to_string();
    let muse_end = screen(u::END_OF_TURN_1_4_10);
    let muse_caret = aterm_phase::muse::composer(&muse_end).expect("Muse Code's caret");
    let mut muse_next = muse_end.clone();
    let said = muse_next
        .iter()
        .position(|r| r.starts_with("◆ "))
        .expect("Muse Code's reply");
    muse_next[said] = muse_next[said].replace("(reply 1)", "(reply 2)");
    let claude_end = ended("Stage 1 is in.");
    let claude_caret = claude_end
        .iter()
        .position(|r| r == "❯")
        .expect("Claude Code's caret");
    let runs = [
        (
            "agy",
            screen(a::BUSY),
            agy_end.clone(),
            agy_drafted(&agy_end, &["a note for later"]),
            agy_next,
        ),
        (
            "muse",
            screen(u::BUSY_1_4_10),
            muse_end.clone(),
            caret_typed(&muse_end, muse_caret),
            muse_next,
        ),
        (
            "claude",
            busy_screen(),
            claude_end.clone(),
            caret_typed(&claude_end, claude_caret),
            ended("Stage 2 is in."),
        ),
    ];
    for (program, busy, end, drafted, next) in runs {
        let points = points_under_a_person(
            &format!("draft-cleared-{program}"),
            program,
            vec![busy.clone(), end.clone(), drafted.clone(), end.clone()],
        );
        assert_eq!(points.len(), 1, "{program}: one point: {points:#?}");
        let points = points_under_a_person(
            &format!("draft-cleared-then-a-turn-{program}"),
            program,
            vec![busy.clone(), end.clone(), drafted, end, busy, next],
        );
        assert_eq!(
            points.len(),
            2,
            "{program}: the next turn's end: {points:#?}"
        );
    }
}

/// agy's turn end under the footer its first ctrl+c draws, then under its idle evidence again.
fn agy_exit_hint_then_evidence() -> (Vec<String>, Vec<String>) {
    let end = screen(aterm_phase::agy::fixtures::END_OF_TURN);
    let hint = agy_footer_left(&end, EXIT_HINT);
    assert_eq!(aterm_phase::agy::phase(&hint), (Phase::Idle, false));
    assert_eq!(aterm_phase::agy::phase(&end), (Phase::Idle, true));
    (hint, end)
}

/// A DUE THAT FALLS ON A FRAME DRAWN FOR ONE READ IS NOT DROPPED (ATERM-KI-1101-2 repair (e)): the
/// person's grace at agy's turn end runs out as the first ctrl+c's exit hint shows for the one read
/// the due makes, which is the same point read without its evidence, so nothing is done there and
/// the point is decided again at the next look, its evidence back (`unauthoritative_look`), and the
/// loop goes on, as in the same run with no hint at the due.
#[test]
fn a_due_falling_on_a_frame_drawn_for_one_read_stands_for_the_next_look() {
    const GRACE: u64 = 30;
    let (hint, end) = agy_exit_hint_then_evidence();
    let run = |passing: bool| {
        let mut screens = vec![screen(aterm_phase::agy::fixtures::BUSY), end.clone()];
        if passing {
            screens.push(hint.clone());
        }
        screens.push(end.clone());
        let mut m = agy_mock(screens);
        // The person keys at the turn's end, and the end stands until their grace runs out.
        m.keyed_at_read = Some(1);
        m.stands.insert(1, Duration::from_secs(GRACE));
        m.vanish_after = Some(4);
        m.exit_when = Some(|m| {
            m.requests
                .iter()
                .any(|r| r.starts_with("send ") || r.starts_with("turn "))
        });
        let mut opts = hosted(600);
        opts.policy.human_grace_s = u32::try_from(GRACE).expect("a grace");
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.setup.wait_step = Duration::from_secs(600);
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let hint_read = m.read_times.iter().any(|(i, _)| passing && *i == 2);
        (
            count(&m, "send") + count(&m, "turn"),
            hint_read,
            lines,
            m.requests,
        )
    };
    let (sends, read, lines, requests) = run(true);
    assert!(read, "the due's read is the hint's: {requests:#?}");
    assert_eq!(sends, 1, "{lines:#?}\n{requests:#?}");
    let (sends, _, lines, requests) = run(false);
    assert_eq!(sends, 1, "{lines:#?}\n{requests:#?}");
}

/// `rows` with the row naming `from` in agy's footer naming `to` there instead.
fn agy_footer_swapped(rows: &[String], from: &str, to: &str) -> Vec<String> {
    let mut out = rows.to_vec();
    let at = out
        .iter()
        .rposition(|r| r.contains(from))
        .expect("the footer's words");
    out[at] = out[at].replace(from, to);
    out
}

/// The loop at agy's turn end (`end`), a person keying as it ends, whose grace runs out as `due`
/// shows for the one read the due makes, then `end` again: the prompts typed, whether the due's
/// read was `due`'s, and the journal's lines.
fn a_due_falling_on(end: &[String], due: &[String]) -> (usize, bool, Vec<String>, Vec<String>) {
    const GRACE: u64 = 30;
    let mut m = agy_mock(vec![
        screen(aterm_phase::agy::fixtures::BUSY),
        end.to_vec(),
        due.to_vec(),
        end.to_vec(),
    ]);
    m.keyed_at_read = Some(1);
    m.stands.insert(1, Duration::from_secs(GRACE));
    m.vanish_after = Some(4);
    m.exit_when = Some(|m| {
        m.requests
            .iter()
            .any(|r| r.starts_with("send ") || r.starts_with("turn "))
    });
    let mut opts = hosted(600);
    opts.policy.human_grace_s = u32::try_from(GRACE).expect("a grace");
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.setup.wait_step = Duration::from_secs(600);
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let read = m.read_times.iter().any(|(i, _)| *i == 2);
    (
        count(&m, "send") + count(&m, "turn"),
        read,
        lines,
        m.requests,
    )
}

/// A DUE THAT FALLS ON A ONE-READ FRAME OF ANOTHER POINT IS NOT DROPPED EITHER (the review of repair
/// (e)): agy's frame torn mid-redraw (its bottom rule and footer not yet drawn, so its transcript
/// runs to the last row), or its reply's last row drawn otherwise for one read, is another key, so
/// nothing is decided on it; the due stands, the handed frame is back at the next look, and that
/// look decides it, as the run with the footer's idle evidence missing for the read does
/// (`a_due_falling_on_a_frame_drawn_for_one_read_stands_for_the_next_look`).
#[test]
fn a_due_falling_on_a_one_read_frame_of_another_point_stands_for_the_next_look() {
    let end = screen(aterm_phase::agy::fixtures::END_OF_TURN);
    let (_, bottom) = aterm_phase::agy::composer_rules(&end).expect("the frame");
    let mut torn = end.clone();
    for r in torn.iter_mut().skip(bottom) {
        r.clear();
    }
    let last = end
        .iter()
        .rposition(|r| r.starts_with("  and ocean floor depth"))
        .expect("the reply's last row");
    let mut reworded = end.clone();
    reworded[last] = "  and ocean floor depth further amplify".to_string();
    let reader = aterm_phase::Program::Agy.reader();
    let key = |rows: &[String]| {
        point_key(
            reader,
            &Turn {
                phase: reader.phase(rows),
                screen: Screen {
                    rows: rows.to_vec(),
                    ..Screen::default()
                },
                timed_out: false,
            },
        )
    };
    for (what, due) in [("a torn frame", torn), ("a reworded row", reworded)] {
        assert_ne!(key(&due), key(&end), "{what}: another point");
        let (sends, read, lines, requests) = a_due_falling_on(&end, &due);
        assert!(read, "{what}: the due's read is the frame's: {requests:#?}");
        assert_eq!(sends, 1, "{what}: {lines:#?}\n{requests:#?}");
    }
}

/// The journal's points (`EVENT idle`) over `screens` under agy, a person's key 2 s old at every
/// read ([`points_under_a_person`]).
fn agy_points(tag: &str, screens: Vec<Vec<String>>) -> Vec<String> {
    points_under_a_person(tag, "agy", screens)
}

/// A FOOTER READ CUT SHORT IS NO NEW POINT, nor its model's return: a read of agy's footer torn
/// mid-repaint names `Gemini 4` of `Gemini 4 Argon · medium`, and the model weighs only at a limit
/// wall, which this turn's end is not. NEGATIVE CONTROL: the same model swapped under the measured
/// quota wall is a new point (`a_model_switched_under_a_limit_wall_is_a_new_point`).
#[test]
fn a_footer_read_cut_short_is_no_new_point() {
    use aterm_phase::agy::fixtures as a;
    let end = screen(a::END_OF_TURN);
    let torn = agy_footer_swapped(&end, "Gemini 4 Argon · medium", "Gemini 4               ");
    assert_eq!(
        (
            aterm_phase::agy::footer_model(&end).as_deref(),
            aterm_phase::agy::footer_model(&torn).as_deref()
        ),
        (Some("Gemini 4 Argon"), Some("Gemini 4"))
    );
    let points = agy_points(
        "footer-cut-short",
        vec![screen(a::BUSY), end.clone(), torn, end],
    );
    assert_eq!(points.len(), 1, "{points:#?}");
}

/// A MODEL SWITCHED UNDER A LIMIT WALL IS A NEW POINT (`/model` under the measured quota wall,
/// `lab::WALL_INDIVIDUAL_QUOTA`, draws nothing above agy's composer): the loop journals it and
/// decides it, the switch weighed at once. Its effort switched is the same point. A point handed on
/// a read whose footer names no model (as a pane under about 40 columns draws it at idle) takes the
/// first model a read of it names, and a switch from that one is a new point; the first named is
/// the point's own.
#[test]
fn a_model_switched_under_a_limit_wall_is_a_new_point() {
    use aterm_phase::agy::fixtures as a;
    let wall = screen(a::lab::WALL_INDIVIDUAL_QUOTA);
    let switched = agy_footer_swapped(&wall, "Claude Opus 5.5", "Gemini 3.1 Pro");
    let effort = agy_footer_swapped(&wall, "· high", "· low");
    let mut unnamed = wall.clone();
    let footer = unnamed
        .iter()
        .rposition(|r| r.contains("Claude Opus 5.5"))
        .expect("the footer");
    unnamed[footer] = "? for shortcuts".to_string();
    assert_eq!(aterm_phase::agy::footer_model(&unnamed), None);
    for (what, then, want) in [
        (
            "the model switched",
            vec![wall.clone(), switched.clone()],
            2,
        ),
        ("its effort switched", vec![wall.clone(), effort], 1),
        (
            "switched from the first named",
            vec![unnamed.clone(), wall.clone(), switched.clone()],
            2,
        ),
        ("the first named", vec![unnamed, switched], 1),
    ] {
        let mut screens = vec![screen(a::BUSY)];
        screens.extend(then);
        let points = agy_points(&format!("wall-{}", what.replace(' ', "-")), screens);
        assert_eq!(points.len(), want, "{what}: {points:#?}");
    }
}

/// A DUE AT A LIMIT WALL THAT FALLS ON A READ NAMING ANOTHER MODEL IS DECIDED ONCE, AT ITS OWN
/// POINT: the person's grace at the measured quota wall runs out as `/model` has switched its
/// footer's model, so the due's read is another point; the next look journals that point and
/// decides it there, never first as the old point and then again as the new one.
/// The same run with the model unswitched decides once, at the wall's own point.
#[test]
fn a_due_at_a_limit_wall_falling_on_another_model_is_decided_once_at_its_own_point() {
    use aterm_phase::agy::fixtures as a;
    const GRACE: u64 = 30;
    let wall = screen(a::lab::WALL_INDIVIDUAL_QUOTA);
    let switched = agy_footer_swapped(&wall, "Claude Opus 5.5", "Gemini 3.1 Pro");
    for (what, due, points) in [("switched", switched, 2), ("unswitched", wall.clone(), 1)] {
        let mut m = agy_mock(vec![screen(a::BUSY), wall.clone(), due.clone(), due]);
        m.keyed_at_read = Some(1);
        m.stands.insert(1, Duration::from_secs(GRACE));
        m.vanish_after = Some(6);
        let (dir, path) = journal_file(&format!("wall-due-{what}"));
        let mut opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..hosted(600)
        };
        opts.policy.human_grace_s = u32::try_from(GRACE).expect("a grace");
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.setup.wait_step = Duration::from_secs(600);
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let lines: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let held = lines
            .iter()
            .position(|l| {
                l.starts_with("WAITING ")
                    && l.ends_with(crate::supervise::policy::turn_end::PERSON_TYPING)
            })
            .expect("the person's hold");
        let after: Vec<&String> = lines[held + 1..]
            .iter()
            .filter(|l| l.starts_with("EVENT limited ") || l.starts_with("WAITING "))
            .collect();
        let decided = after.iter().filter(|l| l.starts_with("WAITING ")).count();
        assert_eq!(
            (
                lines
                    .iter()
                    .filter(|l| l.starts_with("EVENT limited "))
                    .count(),
                decided
            ),
            (points, 1),
            "{what}: {lines:#?}"
        );
        if points == 2 {
            assert!(
                after[0].starts_with("EVENT limited "),
                "{what}: decided at the new point: {lines:#?}"
            );
        }
    }
}
