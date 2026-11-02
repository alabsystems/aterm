// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A point is the turn the program drew, never its composer (ATERM-KI-1101-2): each program's key
//! ([`point_key`]) and submit mark ([`submit_mark`]) read the same with a draft typed as with it
//! cleared, and a point moves with the model agy's footer names ([`model_moved`]).

use super::*;
use aterm_phase::agy;
use aterm_phase::agy::fixtures as a;
use aterm_phase::prompt::fixtures::screen;

/// `rows` as `program`'s reader reads them: its phase, the session's submit count at one.
fn read_turn(program: Program, rows: &[String]) -> Turn {
    Turn {
        phase: program.reader().phase(rows),
        screen: Screen {
            rows: rows.to_vec(),
            submits: Some(1),
            ..Screen::default()
        },
        timed_out: false,
    }
}

fn key_of(program: Program, rows: &[String]) -> String {
    point_key(program.reader(), &read_turn(program, rows))
}

fn mark_of(program: Program, rows: &[String]) -> Option<(u64, u64)> {
    submit_mark(program.reader(), &read_turn(program, rows).screen)
}

/// The row agy's footer is drawn on: the last row with words, under the composer's bottom rule
/// (and under the panel's closing rule where one is drawn).
fn agy_footer(rows: &[String]) -> Option<usize> {
    let (_, bottom) = agy::composer_rules(rows)?;
    let last = rows.iter().rposition(|r| !r.trim().is_empty())?;
    (last > bottom).then_some(last)
}

/// `rows` with agy's footer's left side drawn as `left` in place of what it shows there, its right
/// side (the model, and the tasks after it) where it was.
pub(super) fn agy_footer_left(rows: &[String], left: &str) -> Vec<String> {
    let mut out = rows.to_vec();
    let at = agy_footer(rows).expect("a footer");
    let row = rows[at].trim_end();
    let right = match row.trim_start().rsplit_once("  ") {
        Some((_, right)) => right.trim_start(),
        None if row.starts_with(' ') => row.trim_start(),
        None => "",
    };
    let col = row.chars().count() - right.chars().count();
    out[at] = format!(
        "{left}{}{right}",
        " ".repeat(col.saturating_sub(left.chars().count()))
    );
    out
}

/// `rows` with `lines` typed into agy's composer as agy draws a draft: the caret row and the rows
/// the draft wraps onto (the screen scrolled up a row for each), and the footer's left side blank.
pub(super) fn agy_drafted(rows: &[String], lines: &[&str]) -> Vec<String> {
    let blank = agy_footer_left(rows, "");
    let caret = agy::composer(&blank).expect("the composer");
    let mut out = blank;
    out[caret] = format!("> {}", lines[0]);
    for (k, line) in lines.iter().enumerate().skip(1) {
        out.insert(caret + k, format!("  {line}"));
        out.remove(0);
    }
    out
}

/// Every agy screen the fixtures measured.
fn agy_fixtures() -> Vec<(&'static str, Vec<String>)> {
    use a::lab;
    [
        ("IDLE", a::IDLE),
        ("IDLE_100X30", a::IDLE_100X30),
        ("IDLE_FRESH_NOTICE", a::IDLE_FRESH_NOTICE),
        ("END_OF_TURN", a::END_OF_TURN),
        ("INTERRUPTED", a::INTERRUPTED),
        ("ERROR_TURN", a::ERROR_TURN),
        ("CANCELLED", a::CANCELLED),
        ("DECLINED", a::DECLINED),
        ("REJECTED", a::REJECTED),
        ("QUESTION", a::QUESTION),
        ("lab::IDLE", lab::IDLE),
        ("lab::END_OF_TURN", lab::END_OF_TURN),
        ("lab::WORKER_FIRST_PROMPT", lab::WORKER_FIRST_PROMPT),
        ("lab::PERSON_PARAGRAPHS", lab::PERSON_PARAGRAPHS),
        (
            "lab::PERSON_PARAGRAPHS_LONG_REPLY",
            lab::PERSON_PARAGRAPHS_LONG_REPLY,
        ),
        ("lab::REPLY_WRAPPED_50", lab::REPLY_WRAPPED_50),
        ("lab::INTERRUPTED", lab::INTERRUPTED),
        ("lab::QUESTION_ANSWERED", lab::QUESTION_ANSWERED),
        ("lab::ERROR_429", lab::ERROR_429),
        ("lab::ERROR_401", lab::ERROR_401),
        ("lab::ERROR_400_KEY", lab::ERROR_400_KEY),
        ("lab::ERROR_503", lab::ERROR_503),
        ("lab::ERROR_500", lab::ERROR_500),
        ("lab::ERROR_403_DISABLED", lab::ERROR_403_DISABLED),
        ("lab::ERROR_403", lab::ERROR_403),
        ("lab::ERROR_404", lab::ERROR_404),
        ("lab::ERROR_400_LOCATION", lab::ERROR_400_LOCATION),
        ("lab::ERROR_504", lab::ERROR_504),
        ("lab::ERROR_429_BARE", lab::ERROR_429_BARE),
        ("lab::ERROR_429_PER_MINUTE", lab::ERROR_429_PER_MINUTE),
        ("lab::ERROR_429_LONG", lab::ERROR_429_LONG),
        ("lab::ERROR_429_INDIVIDUAL", lab::ERROR_429_INDIVIDUAL),
        ("lab::BG_TASK_ENDED", lab::BG_TASK_ENDED),
        ("lab::BG_TASK", lab::BG_TASK),
        ("lab::BG_TIMER", lab::BG_TIMER),
        ("lab::BG_SUBAGENT", lab::BG_SUBAGENT),
        ("lab::BG_PANEL_OFF_TASK", lab::BG_PANEL_OFF_TASK),
        ("lab::ERROR_429_INDIVIDUAL", lab::ERROR_429_INDIVIDUAL),
        ("lab::WALL_INDIVIDUAL_QUOTA", lab::WALL_INDIVIDUAL_QUOTA),
    ]
    .into_iter()
    .map(|(name, text)| (name, screen(text)))
    .collect()
}

/// The agy screens a person can type a draft over: the composer framed, its idle evidence
/// (`? for shortcuts`) on the footer's left.
fn agy_draftable() -> Vec<(&'static str, Vec<String>)> {
    let found: Vec<(&'static str, Vec<String>)> = agy_fixtures()
        .into_iter()
        .filter(|(_, rows)| {
            agy::phase(rows).1
                && agy_footer(rows).is_some_and(|f| rows[f].starts_with("? for shortcuts"))
        })
        .collect();
    assert!(
        found.len() >= 25,
        "the walk reads the fixtures it names: {:?}",
        found.iter().map(|(n, _)| *n).collect::<Vec<_>>()
    );
    found
}

/// AGY'S POINT IS NOT ITS COMPOSER: a draft typed (one line, or wrapped onto a second row) and
/// cleared over every measured agy frame a person can type at reads one key and no submission over
/// moved rows (one submit mark where the draft scrolls no row off), the measured pairs
/// (`IDLE`/`DRAFT`, `lab::IDLE`/`lab::DRAFT`) included. NEGATIVE CONTROLS: the
/// rows Claude Code's rule keeps reach the composer on each frame (so the key it gave moved with
/// each draft), and a real end moves the key.
#[test]
fn agys_point_is_not_its_composer() {
    let agy = Program::Agy;
    for (idle, drafted) in [(a::IDLE, a::DRAFT), (a::lab::IDLE, a::lab::DRAFT)] {
        let (idle, drafted) = (screen(idle), screen(drafted));
        assert_eq!(agy::phase(&idle), (Phase::Idle, true));
        assert_eq!(agy::phase(&drafted), (Phase::Idle, true));
        assert_eq!(key_of(agy, &idle), key_of(agy, &drafted), "{drafted:#?}");
        assert_eq!(mark_of(agy, &idle), mark_of(agy, &drafted));
    }
    for (name, rows) in agy_draftable() {
        let (top, _) = agy::composer_rules(&rows).expect("the frame");
        assert!(
            last_said_index(&rows).is_some_and(|k| k >= top),
            "{name}: Claude Code's rule reads the composer here: {rows:#?}"
        );
        let key = key_of(agy, &rows);
        let mark = mark_of(agy, &rows);
        for lines in [
            &["a note for later"][..],
            &["a note for later that runs on", "and a second line"][..],
        ] {
            let drafted = agy_drafted(&rows, lines);
            assert_eq!(
                agy::composer_draft(&drafted).map(|(_, l)| l.len()),
                Some(lines.len()),
                "{name}: the draft drawn: {drafted:#?}"
            );
            assert_eq!(
                agy::phase(&drafted),
                agy::phase(&rows),
                "{name}: a draft over a blank footer is the same read: {drafted:#?}"
            );
            // A draft that wraps scrolls the screen's top row off: the key's rows are the turn's
            // last `KEY_ROWS` with words, so a screen showing no more than those moves its key, and
            // its mark, as Claude Code's does.
            let worded = rows[..top].iter().filter(|r| !r.trim().is_empty()).count();
            if lines.len() == 1 || worded > KEY_ROWS {
                assert_eq!(key_of(agy, &drafted), key, "{name} {lines:?}");
            }
            if lines.len() == 1 {
                assert_eq!(mark_of(agy, &drafted), mark, "{name} {lines:?}");
            }
            assert!(
                !submitted_over(mark, agy.reader(), &read_turn(agy, &drafted).screen),
                "{name} {lines:?}"
            );
        }
        // A permission mode's hint in the empty composer, the footer as it was.
        let mut hinted = rows.clone();
        let caret = agy::composer(&hinted).expect("the caret");
        hinted[caret] = "> Plan mode: research & plan only (shift+tab to cycle)".to_string();
        assert_eq!(agy::phase(&hinted), agy::phase(&rows), "{name}");
        assert_eq!(key_of(agy, &hinted), key, "{name}: a mode's hint");
    }
    let end = screen(a::END_OF_TURN);
    let last = end
        .iter()
        .rposition(|r| r.starts_with("  and ocean floor depth"))
        .expect("the reply's last row");
    let mut said = end.clone();
    said[last] = "  and ocean floor depth shape these rhythms further.".to_string();
    assert_ne!(key_of(agy, &end), key_of(agy, &said), "the reply moved");
    assert_ne!(
        key_of(agy, &screen(a::BUSY)),
        key_of(agy, &end),
        "busy to its end"
    );
    assert_ne!(
        key_of(agy, &screen(a::lab::BG_TASK)),
        key_of(agy, &screen(a::lab::BG_TASK_ENDED)),
        "the background task ended"
    );
}

/// AGY'S POINT HOLDS THROUGH ITS EVIDENCE: its idle is evidence by what its footer and caret row
/// draw, under the same turn rows, so each pair reads (Idle, no evidence) then (Idle, evidence)
/// under one turn and one key, and the loop decides the point again once the evidence is back
/// (`unauthoritative_look`: `an_agy_unauthoritative_idle_clearing_on_the_same_transcript_decides_again`).
/// NEGATIVE CONTROL: the turn's last row reworded under the evidence is another point.
#[test]
fn agys_point_holds_through_its_evidence() {
    let agy = Program::Agy;
    let shortcuts = |rows: &[String]| agy_footer_left(rows, "? for shortcuts");
    let exit = screen(a::EXIT_CONFIRM);
    let exit_d = screen(a::lab::EXIT_CONFIRM_CTRL_D);
    let ctrl_c = screen(a::lab::CTRL_C_DRAFT);
    let end = screen(a::lab::END_OF_TURN);
    let menu = screen(a::lab::SLASH_MENU);
    let (end_top, _) = agy::composer_rules(&end).expect("the frame");
    let (menu_top, _) = agy::composer_rules(&menu).expect("the menu's frame");
    let mut menu_over_end = end[..end_top].to_vec();
    menu_over_end.extend(menu[menu_top..].iter().cloned());
    let drafted = screen(a::DRAFT);
    let mut emptied = drafted.clone();
    emptied[agy::composer(&drafted).expect("the caret")] = ">".to_string();
    let pairs = [
        ("exit hint", exit.clone(), shortcuts(&exit)),
        ("ctrl+d exit hint", exit_d.clone(), shortcuts(&exit_d)),
        (
            "ctrl+c over a draft",
            ctrl_c.clone(),
            agy_footer_left(&ctrl_c, ""),
        ),
        ("the / menu open", menu_over_end, end),
        ("an empty composer", emptied, screen(a::IDLE)),
    ];
    for (what, left, right) in pairs {
        assert_eq!(agy::phase(&left), (Phase::Idle, false), "{what}: {left:#?}");
        assert_eq!(
            agy::phase(&right),
            (Phase::Idle, true),
            "{what}: {right:#?}"
        );
        let (lt, _) = agy::composer_rules(&left).expect("the frame");
        let (rt, _) = agy::composer_rules(&right).expect("the frame");
        assert_eq!(left[..lt], right[..rt], "{what}: one turn's rows");
        assert_eq!(key_of(agy, &left), key_of(agy, &right), "{what}");
        let last = (0..rt)
            .rev()
            .find(|&i| !right[i].trim().is_empty())
            .expect("a turn row");
        let mut reworded = right.clone();
        reworded[last] = format!("{} and more", right[last].trim_end());
        assert_ne!(key_of(agy, &left), key_of(agy, &reworded), "{what}");
    }
}

fn model_of(rows: &[String]) -> Option<String> {
    named_model(Program::Agy.reader(), rows)
}

fn models_moved(then: &[String], now: &[String]) -> bool {
    model_moved(model_of(then).as_deref(), model_of(now).as_deref())
}

/// AGY'S POINT MOVES WITH THE MODEL ITS FOOTER NAMES, AND WITH NO MORE OF ITS FOOTER: `/model` under
/// the measured quota wall (`lab::WALL_INDIVIDUAL_QUOTA`, met under one model) draws nothing above
/// the composer, so one key reads both frames and the model moved ([`model_moved`]); its effort,
/// its task count and a draft typed over the wall move neither. NEGATIVE CONTROL: one turn's rows
/// in every frame here, and each one names a model.
#[test]
fn agys_point_moves_with_its_model() {
    let agy = Program::Agy;
    let wall = screen(a::lab::WALL_INDIVIDUAL_QUOTA);
    let footer = agy_footer(&wall).expect("a footer");
    let swap = |from: &str, to: &str| {
        let mut out = wall.clone();
        assert!(out[footer].contains(from), "{from}: {}", out[footer]);
        out[footer] = out[footer].replace(from, to);
        out
    };
    let (top, _) = agy::composer_rules(&wall).expect("the frame");
    let key = key_of(agy, &wall);
    let switched = swap("Claude Opus 5.5", "Gemini 3.1 Pro");
    assert_eq!(wall[..top], switched[..top], "one turn's rows");
    assert_eq!(key_of(agy, &switched), key, "the key is the turn's");
    assert!(models_moved(&wall, &switched), "the model switched");
    for (what, same) in [
        ("its effort", swap("· high", "· low")),
        ("its tasks", swap(" · 1 task(s) · /tasks", "")),
        ("a draft", agy_drafted(&wall, &["a note for later"])),
    ] {
        assert_eq!(key_of(agy, &same), key, "{what}: {}", same[footer]);
        assert!(model_of(&same).is_some(), "{what}");
        assert!(!models_moved(&wall, &same), "{what}: {}", same[footer]);
    }
}

/// A DRAFT OR A PASTE AT A NARROW PANE IS NO NEW POINT EITHER, measured on agy 1.4.12's own frames: at
/// 30 and 36 columns its idle footer draws `? for shortcuts` alone and a draft's draws the model
/// alone, at 44 both; 60 lines pasted are one composer row (`[Pasted text #1 +60 lines]`) under a
/// draft's footer. Each pair reads one key and one submit mark, and no model moved (the idle frame
/// at 30 and 36 columns names none, so nothing compares). NEGATIVE CONTROL: each pair's footer
/// differs, the narrow idle frames name no model and their drafts one.
#[test]
fn agys_point_at_a_narrow_pane_and_under_a_paste_is_its_turn() {
    use a::lab;
    let agy = Program::Agy;
    for (what, idle, typed, named_at_idle) in [
        (
            "30 columns",
            lab::NARROW_30_END,
            lab::NARROW_30_DRAFT,
            false,
        ),
        (
            "36 columns",
            lab::NARROW_36_END,
            lab::NARROW_36_DRAFT,
            false,
        ),
        ("44 columns", lab::NARROW_44_END, lab::NARROW_44_DRAFT, true),
        ("a tall paste", lab::LONG_REPLY_END, lab::TALL_PASTE, true),
    ] {
        let (idle, typed) = (screen(idle), screen(typed));
        let (f, g) = (
            agy_footer(&idle).expect("a footer"),
            agy_footer(&typed).expect("a footer"),
        );
        assert_ne!(idle[f], typed[g], "{what}: the footers differ");
        assert_eq!(
            model_of(&idle).is_some(),
            named_at_idle,
            "{what}: {}",
            idle[f]
        );
        assert!(model_of(&typed).is_some(), "{what}: {}", typed[g]);
        assert_eq!(key_of(agy, &idle), key_of(agy, &typed), "{what}");
        assert_eq!(mark_of(agy, &idle), mark_of(agy, &typed), "{what}");
        assert!(!models_moved(&idle, &typed), "{what}");
    }
}

/// A DRAFT TYPED AND CLEARED IS NO NEW POINT UNDER ANY PROGRAM: each program whose reader finds a
/// composer has a measured frame with a draft and without one here, and both read one key and one
/// submit mark. A program with no such reader keeps the screen's rows (none to tell a composer by),
/// so its reader must find no composer on any program's frames here, Grok Build's own measured
/// frames among them (its drafts too; no Codex frame is measured): one that learns to will fail
/// this walk until its pairs join it. NEGATIVE CONTROL: each pair's composer differs, a draft in
/// one and none in the other.
#[test]
fn a_draft_typed_and_cleared_is_no_new_point_under_any_program() {
    use aterm_phase::{kimi, muse};
    let claude = screen(aterm_phase::prompt::fixtures::API_ERROR_TWICE_MEASURED);
    let typed_at = |rows: &[String], caret: usize, glyph: &str| {
        let mut out = rows.to_vec();
        out[caret] = format!("{glyph} a note for later");
        out
    };
    let claude_caret = aterm_phase::phase::composer_index(&claude).expect("Claude Code's caret");
    let muse_end = screen(muse::fixtures::END_OF_TURN_1_4_10);
    let muse_caret = muse::composer(&muse_end).expect("Muse Code's caret");
    let pairs_of = |program: Program| -> Option<Vec<(Vec<String>, Vec<String>)>> {
        match program {
            Program::Claude => Some(vec![(claude.clone(), typed_at(&claude, claude_caret, "❯"))]),
            Program::Agy => Some(vec![
                (screen(a::IDLE), screen(a::DRAFT)),
                (screen(a::lab::IDLE), screen(a::lab::DRAFT)),
                (
                    screen(a::END_OF_TURN),
                    agy_drafted(&screen(a::END_OF_TURN), &["a note for later"]),
                ),
            ]),
            Program::Muse => Some(vec![(
                muse_end.clone(),
                typed_at(&muse_end, muse_caret, "❯"),
            )]),
            Program::Kimi => Some(vec![(
                screen(kimi::fixtures::IDLE),
                screen(kimi::fixtures::DRAFT),
            )]),
            Program::Codex | Program::Grok | Program::Generic => None,
        }
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
    let grok: Vec<Vec<String>> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("grok-"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .map(|text| screen(&text))
        .collect();
    assert!(grok.len() > 20, "Grok Build's frames: {}", grok.len());
    let every: Vec<Vec<String>> = Program::ALL
        .into_iter()
        .filter_map(pairs_of)
        .flatten()
        .flat_map(|(i, d)| [i, d])
        .chain(grok)
        .collect();
    for program in Program::ALL {
        let reader = program.reader();
        let Some(pairs) = pairs_of(program) else {
            for rows in &every {
                assert!(
                    reader.composer(rows).is_none() && own_turn_end(reader, rows).is_none(),
                    "{program:?}'s reader finds a composer: give it a measured pair here"
                );
            }
            continue;
        };
        for (idle, drafted) in pairs {
            let typed = |rows: &[String]| {
                reader
                    .composer(rows)
                    .map(|(_, lines)| lines.concat())
                    .or_else(|| own_turn_end(reader, rows).map(|top| rows[top..].concat()))
                    .map(|t| t.trim().to_string())
            };
            assert!(
                typed(&drafted).is_some_and(|t| !t.is_empty()) && typed(&idle) != typed(&drafted),
                "{program:?}: the draft is in the composer: {drafted:#?}"
            );
            assert_eq!(
                key_of(program, &idle),
                key_of(program, &drafted),
                "{program:?}: {drafted:#?}"
            );
            assert_eq!(
                mark_of(program, &idle),
                mark_of(program, &drafted),
                "{program:?}"
            );
        }
    }
}

/// The key and submit mark Claude Code's rule gave every program's point before ATERM-KI-1101-2:
/// the turn's rows end under the last said row, and nothing drawn under them joins the key.
fn claude_rule(reader: &dyn ScreenReader, turn: &Turn) -> (String, Option<(u64, u64)>) {
    let rows = &turn.screen.rows;
    let upto = last_said_index(rows).map_or(0, |k| k + 1).min(rows.len());
    let mark = turn.screen.submits.map(|n| {
        (
            n,
            crate::supervise::policy::turn_end::screen_mark(&rows[..upto]),
        )
    });
    if turn.phase == Phase::Prompt
        && let Some(p) = reader.prompt(rows)
    {
        return (box_key(&p, rows), mark);
    }
    let mut key = format!(
        "{} {}\n{}\n",
        turn.phase.name(),
        said_summary(reader, turn),
        status_row(rows).unwrap_or("")
    );
    let span = (turn.phase == Phase::Prompt)
        .then(|| prompt_box_span(rows))
        .flatten();
    push_words_above(&mut key, &rows[..span.map_or(upto, |(a, _)| a)]);
    if let Some((a, b)) = span {
        for r in &rows[a..=b] {
            key.push_str(r.trim_end());
            key.push('\n');
        }
    }
    (key, mark)
}

/// EVERY OTHER POINT KEEPS CLAUDE CODE'S RULE BYTE FOR BYTE: over every measured screen of every
/// program in the fixtures, each read whose key is not cut at the program's own turn end
/// ([`own_turn_end`]) gets the key and the submit mark that rule gave, and Claude Code, Muse Code, Codex, Grok Build and a generic program
/// never cut. NEGATIVE CONTROL: agy's and Kimi Code's keys, cut at their frames, differ from it on
/// their own screens.
#[test]
fn every_other_programs_point_keeps_claude_codes_rule() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
    let mut screens: Vec<(String, Vec<String>)> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(e.path()).ok()?;
            name.ends_with(".txt").then(|| (name, screen(&text)))
        })
        .collect();
    screens.sort();
    assert!(screens.len() > 1000, "{} screens", screens.len());
    let agy_frames = screens.iter().filter(|(name, _)| name.starts_with("agy-"));
    for (name, rows) in agy_frames.clone() {
        // Only `NO_EVIDENCE_HOLD` absorbs a transient agy frame: its reader draws none torn.
        assert!(!Program::Agy.reader().frame_torn(rows), "{name}");
    }
    assert!(agy_frames.count() > 80);
    let mut cut = std::collections::HashMap::<Program, usize>::new();
    for program in Program::ALL {
        let reader = program.reader();
        for (name, rows) in &screens {
            let turn = read_turn(program, rows);
            let (key, mark) = claude_rule(reader, &turn);
            let now = (point_key(reader, &turn), submit_mark(reader, &turn.screen));
            let own = own_turn_end(reader, rows).is_some();
            match program {
                Program::Agy | Program::Kimi if own => {
                    *cut.entry(program).or_default() += usize::from(now != (key, mark));
                }
                Program::Agy
                | Program::Kimi
                | Program::Codex
                | Program::Claude
                | Program::Muse
                | Program::Grok
                | Program::Generic => {
                    assert!(!own, "{program:?} on {name}");
                    assert_eq!(now, (key, mark), "{program:?} on {name}");
                }
            }
        }
    }
    assert!(
        cut.get(&Program::Agy).copied().unwrap_or(0)
            + cut.get(&Program::Kimi).copied().unwrap_or(0)
            > 100,
        "agy's and Kimi Code's frames are cut: {cut:?}"
    );
}
