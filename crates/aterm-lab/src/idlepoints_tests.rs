// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn decision(seq: u64, rows: &[&str], journal: &str) -> show::Decision {
    show::Decision {
        door: "decide.turn_end".to_string(),
        item: 0,
        rule: None,
        outcome: "Nothing".to_string(),
        seq,
        screen_record: Some(1),
        screen: Some(rows.iter().map(|r| (*r).to_string()).collect()),
        act: None,
        reply: None,
        journal: Some(journal.to_string()),
    }
}

#[test]
fn a_bare_status_row_is_a_point_with_no_evidence() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let bare = screen(u::BUSY_FIRST_FRAME_CYCLED);
    let ended = screen(u::END_OF_TURN_1_4_10);
    let bare_refs: Vec<&str> = bare.iter().map(String::as_str).collect();
    let ended_refs: Vec<&str> = ended.iter().map(String::as_str).collect();
    let pts = points(
        &[
            decision(7, &bare_refs, "EVENT idle seq=7 -"),
            decision(9, &ended_refs, "EVENT idle seq=9 Nothing left."),
        ],
        Some(Program::Muse.reader()),
    );
    assert_eq!(pts.len(), 2);
    assert!(!pts[0].evidence, "{:?}", pts[0]);
    assert!(pts[0].nameless);
    assert!(pts[1].evidence, "{:?}", pts[1]);
    assert!(!pts[1].nameless);
    let text = report("t", &pts, 1);
    assert!(
        text.contains("t: 2 points, 2 idle, 1 with no evidence, 1 naming no last line"),
        "{text}"
    );
    assert!(text.contains("NO EVIDENCE seq=7"), "{text}");
    assert!(!text.contains("seq=9 outcome"), "{text}");
}

#[test]
fn the_title_is_the_last_osc_0_or_2() {
    assert_eq!(osc_title("\x1b]0;⠙ proj\x07"), Some("⠙ proj".to_string()));
    assert_eq!(
        osc_title("a\x1b]2;one\x1b\\b\x1b]0;proj\x07c"),
        Some("proj".to_string())
    );
    assert_eq!(osc_title("\x1b]633;P;Cwd=/x\x07\x1b]8;;http://x\x07"), None);
    assert_eq!(osc_title("plain"), None);
}

const HM4_JOURNAL: &str = include_str!("fixtures/muse-1.4.10-R6427.1-lab-hm4-t2.journal.jsonl");
const HM4_SESSION: &str = include_str!("fixtures/muse-1.4.10-R6427.1-lab-hm4-t2.session.jsonl");

#[test]
fn a_tab_and_its_muse_session_are_paired_by_the_name_both_say() {
    assert_eq!(
        subject_named(
            "#41 status → 0 OK schema=1 sid=7 subject=land-d subject_source=pin observed=true"
        )
        .as_deref(),
        Some("land-d")
    );
    assert_eq!(
        subject_named("0 OK schema=1 sid=7 subject=- subject_source=cwd"),
        None
    );
    assert_eq!(
        session_project(
            r#"{"cwd":"/private/tmp/at.xdoA/proj/land-i"} {"path":"/private/tmp/at.xdoA/proj/land-i/README.md"} {"x":"proj/"}"#
        )
        .as_deref(),
        Some("land-i")
    );
    let named = |n: &str| Some(n.to_string());
    let tabs = [
        (1.0, named("cruise-n")),
        (2.0, named("land-a")),
        (4.0, named("instant-land")),
    ];
    let sessions = [
        (1.5, named("cruise-n")),
        (2.5, named("land-a")),
        (3.0, named("off")),
        (4.5, named("instant-land")),
    ];
    assert_eq!(
        pair(&tabs, &sessions),
        vec![(0, 0, true), (1, 1, true), (2, 3, true)]
    );
    let tabs = [(1.0, None), (2.0, named("proj")), (3.0, named("proj"))];
    let sessions = [
        (1.5, named("proj")),
        (2.5, named("proj")),
        (3.5, named("proj")),
    ];
    assert_eq!(
        pair(&tabs, &sessions),
        vec![(0, 0, false), (1, 1, false), (2, 2, false)]
    );
}

#[test]
fn a_point_in_muses_end_of_turn_gate_is_told_from_the_journals() {
    let (first, runs) = muse_runs(HM4_SESSION);
    assert_eq!(first, Some(1_792_687_536_409.791));
    assert_eq!(runs.len(), 11);
    assert!(
        runs.iter()
            .all(|r| r.committed.is_some() && r.terminal.is_some())
    );
    assert_eq!(runs[1].prompt, "", "round 2 drained a queued input");
    assert_eq!(runs[5].eot_gate_ms, Some(1349));
    let (t0, points) = journal_points(HM4_JOURNAL);
    assert_eq!(t0, Some(1_792_687_537_352.0));
    assert_eq!(points.len(), 11);
    let (text, inside) = gate_report("t2", &points, &runs);
    assert_eq!(inside, 2, "{text}");
    assert!(
        text.contains("t2: 11 replies (10 with an end-of-turn gate, 1171 to 1625 ms), 11 points: 8 after a run's end"),
        "{text}"
    );
    assert!(
        text.contains("2 IN THE GATE, 0 MID-REPLY, 1 before any run"),
        "{text}"
    );
    assert!(
        text.contains("seq=15031 IN THE GATE 0.119 s after"),
        "{text}"
    );
    assert!(
        text.contains("seq=76024 IN THE GATE 1.082 s after"),
        "{text}"
    );
    let later: Vec<JournalPoint> = points
        .iter()
        .map(|p| JournalPoint {
            t: p.t + 1700.0,
            ..p.clone()
        })
        .collect();
    let (text, inside) = gate_report("t2", &later, &runs);
    assert_eq!(inside, 0, "{text}");
    assert!(text.contains("10 after a run's end"), "{text}");
}

/// Claude Code 2.1.311's status-line reply (lab `mr`, MEASURED frames, after a spinner frame
/// constructed over the first of them) draws no busy sign once its first line is up: its
/// frames read busy by their running title, so the one turn end is the done frame. NEGATIVE
/// CONTROL: the titles stripped, the end opens at the first mid-reply frame.
#[test]
fn a_status_line_reply_ends_at_its_done_frame_by_its_title() {
    use aterm_phase::prompt::fixtures::{
        STATUSLINE_MIDREPLY_2_1_311, STATUSLINE_REPLY_DONE_2_1_311, screen as rows_of,
    };
    let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
    let done = rows_of(STATUSLINE_REPLY_DONE_2_1_311);
    let (top, _) = aterm_phase::phase::composer_rules(&mid).expect("a composer");
    let mut spinning = mid.clone();
    spinning.insert(
        top,
        "\u{2736} Deliberating\u{2026} (4s \u{00B7} esc to interrupt)".to_string(),
    );
    let claude = Program::Claude.reader();
    let ends = |titles: [Option<&str>; 4]| -> Vec<usize> {
        let text = halfdraw::frames_cast(&[
            (spinning.as_slice(), titles[0]),
            (mid.as_slice(), titles[1]),
            (mid.as_slice(), titles[2]),
            (done.as_slice(), titles[3]),
        ]);
        let cast = halfdraw::parse_cast(&text).expect("a cast");
        spells(&cast, claude)
            .iter()
            .filter(|s| !s.inside)
            .map(|s| s.event)
            .collect()
    };
    assert_eq!(
        ends([
            Some("\u{25D0} Claude Code"),
            Some("\u{25D0} Claude Code"),
            Some("\u{25D1} Claude Code"),
            Some("\u{2733} Claude Code"),
        ]),
        [3],
        "the done frame"
    );
    assert_eq!(ends([None; 4]), [1], "CONTROL: the first mid-reply frame");
}

const EOT_GATE: &str = include_str!("fixtures/muse-1.4.10-R6427.1-lab-eot-gate.cast");

/// Muse Code's end-of-turn gate (lab `eot-gate`, MEASURED): its last reply stands under its
/// spinner title for 0.5 to 1 s before the title rests. The scan reads the title beside the
/// screen, so the turn's end opens where the title rests; the screen alone (titles stripped)
/// opens it that gate earlier, and the gate is the time the end reports its title rested
/// after the end drew.
#[test]
fn a_muse_turn_end_is_told_by_its_resting_title() {
    let cast = halfdraw::parse_cast(EOT_GATE).expect("the cast");
    let muse = Program::Muse.reader();
    let all = spells(&cast, muse);
    let end = all
        .iter()
        .rfind(|s| s.evidence && !s.inside)
        .expect("the turn's end");
    let title = end.title.as_deref().expect("a title");
    assert!(muse.title_runs(title) == Some(false), "{title:?}");
    let rested = end.title_rested.expect("the title rested");
    assert!((0.5..1.0).contains(&rested), "after the end drew: {rested}");
    assert!(
        end.rows
            .iter()
            .any(|r| r.contains("Hi! I am the local lab stub")),
        "{:?}",
        end.rows
    );
    let text = spell_report("c", &all, 1);
    assert!(
        text.contains(&format!(
            "1 of 1 turn ends saw the title rest (latest {rested:.3} s after the end drew)"
        )),
        "{text}"
    );
    let mut untitled = halfdraw::parse_cast(EOT_GATE).expect("the cast");
    for e in &mut untitled.events {
        while let Some(at) = e.data.find("\x1b]0;") {
            let close = e.data[at..]
                .find('\x07')
                .map_or(e.data.len(), |c| at + c + 1);
            e.data.replace_range(at..close, "");
        }
    }
    let bare = spells(&untitled, muse);
    assert!(
        bare.iter()
            .all(|s| s.title.is_none() && s.title_rested.is_none())
    );
    assert!(!spell_report("c", &bare, 1).contains("title:"));
    let screen_end = bare
        .iter()
        .rfind(|s| s.evidence && !s.inside)
        .expect("the screen's end");
    let gate = end.at - screen_end.at;
    assert!(
        (rested - gate).abs() < 1e-9,
        "the gate the title holds: {gate}, reported {rested}"
    );
    // Claude Code's reader reads its own title, in which Muse Code's spinner is no run: a
    // spell with a title rests where it opened.
    let claude = spells(&cast, Program::Claude.reader());
    assert!(
        claude
            .iter()
            .all(|s| s.title.is_none() || s.title_rested.is_some_and(|r| r <= 0.0))
    );
}
