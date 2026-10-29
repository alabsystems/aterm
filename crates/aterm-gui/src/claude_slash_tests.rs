// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end tests for `crate::claude_slash`.

use std::time::{Duration, Instant};

use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::harness::lights::{self, Light};
use aterm_primer::CLAUDE_TOP_EFFORT_KEY as KEY;
use aterm_window::event::{ElementState, MouseButton};

use super::gesture_tests::*;
use super::{Fact, FactNotice};
use crate::autopilot_menu::MenuRow;
use crate::claude_slash::{
    ECHO, NOTICE_SHOWN, QUIET, REFUSAL_SHOWN, Slash, SlashEnd, SlashRefusal, Stage, key_capitalised,
};
use crate::input::{InputEvent, Source};
use crate::{App, WindowId};

const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
const MANUAL_BUSY: &str = "  \u{23F8} manual mode on \u{00B7} esc to interrupt";
const CARET: usize = 21;
const TOP: usize = 20;

fn claude(app: &App, session: u64) {
    claude_on(app, session, "Opus 5.5");
}

fn claude_on(app: &App, session: u64, model: &str) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_claude_footer(
        PGID,
        None,
        Some(FooterFacts {
            model: Some(model.into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            version: Some("2.1.304".into()),
            ..FooterFacts::default()
        }),
    );
    timeline.set_agent_reader_for_test(true);
}

fn rig() -> (App, WindowId, u64, std::os::unix::net::UnixStream) {
    let (app, wid, session, reader) = app();
    let sink = &app.pool.get(session).expect("session").ctx.sink;
    aterm_pty::set_nonblocking(sink.master(), true).expect("a non-blocking test master");
    sink.note_master_nonblocking(true);
    claude(&app, session);
    (app, wid, session, reader)
}

fn idle() -> (App, WindowId, u64, std::os::unix::net::UnixStream) {
    let (app, wid, session, reader) = rig();
    draw_claude(&app, session, MANUAL);
    (app, wid, session, reader)
}

fn echo(app: &App, session: u64, caret: usize, text: &str) {
    let col = 3 + text.chars().count();
    feed(
        app,
        session,
        format!(
            "\x1b[{row};1H\x1b[2K\u{276F} {text}\x1b[{row};{col}H",
            row = caret + 1
        )
        .as_bytes(),
    );
}

fn answered(app: &App, session: u64, top: usize, tag: bool) {
    let w = cols(app, session);
    let rule = if tag {
        format!(
            "{} {KEY} \u{2500}",
            "\u{2500}".repeat(w - KEY.chars().count() - 3)
        )
    } else {
        "\u{2500}".repeat(w)
    };
    feed(
        app,
        session,
        format!(
            "\x1b[{t};1H\x1b[2K{rule}\x1b[{c};1H\x1b[2K\u{276F} \x1b[{c};3H",
            t = top + 1,
            c = top + 2
        )
        .as_bytes(),
    );
}

fn draw_measured(
    app: &mut App,
    wid: WindowId,
    session: u64,
    text: &str,
    edit: impl FnOnce(&mut Vec<String>),
) -> Option<usize> {
    assert!(app.apply_term_resize(wid, 40, 120));
    let mut rows: Vec<String> = text
        .lines()
        .skip(1)
        .map(|l| {
            l.replace("Workspace", &key_capitalised())
                .replace("workspace", KEY)
        })
        .collect();
    edit(&mut rows);
    let caret = aterm_phase::phase::composer_rules(&rows)
        .and_then(|_| aterm_phase::phase::composer_draft(&rows))
        .map(|(caret, _)| caret);
    let mut screen = String::from("\x1b[?2004h\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        screen.push_str(&format!("\x1b[{};1H{row}", i + 1));
    }
    let at = caret.map_or((rows.len(), 1), |c| (c + 1, 3));
    screen.push_str(&format!("\x1b[{};{}H", at.0, at.1));
    feed(app, session, screen.as_bytes());
    caret
}

fn stage(app: &App, session: u64) -> Option<Stage> {
    app.claude_facts
        .get(&session)
        .and_then(|f| f.press.as_ref())
        .map(|p| p.stage)
}

fn ended(app: &App, session: u64) -> Option<SlashEnd> {
    app.claude_facts
        .get(&session)
        .and_then(|f| f.ended.as_ref())
        .map(|(_, end, _)| *end)
}

fn said(app: &App, wid: WindowId, session: u64) -> Option<String> {
    app.windows[&wid]
        .claude_lights
        .sessions
        .get(&session)
        .and_then(|s| s.fact_notice.as_ref())
        .map(|(notice, _)| notice.text().to_owned())
}

fn rest(slash: Slash) -> Vec<u8> {
    slash.text().as_bytes()[1..].to_vec()
}

fn menu_row(app: &App, key: &str) -> usize {
    app.autopilot_menu
        .as_ref()
        .expect("a menu is open")
        .rows
        .iter()
        .position(|row| matches!(row, MenuRow::Item { act, .. } if act.key() == key))
        .unwrap_or_else(|| panic!("the menu offers {key}"))
}

fn menu_notes(app: &App) -> Vec<String> {
    app.autopilot_menu
        .as_ref()
        .map(|m| {
            m.rows
                .iter()
                .filter_map(|row| match row {
                    MenuRow::Note { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn menu_offers(app: &App, part: &str) -> bool {
    app.autopilot_menu.as_ref().is_some_and(|m| {
        m.rows
            .iter()
            .any(|row| matches!(row, MenuRow::Item { act, .. } if act.key().contains(part)))
    })
}

fn through_enter(
    app: &mut App,
    wid: WindowId,
    session: u64,
    reader: &mut std::os::unix::net::UnixStream,
    slash: Slash,
) -> Instant {
    app.press_slash(wid, session, slash)
        .expect("the press begins");
    assert_eq!(pty(reader, 1), b"/");
    echo(app, session, CARET, "/");
    let at = Instant::now();
    app.advance_slash_presses(at);
    assert_eq!(pty(reader, rest(slash).len()), rest(slash));
    echo(app, session, CARET, &slash.text());
    app.advance_slash_presses(at);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(pty(reader, 1), b"\r", "the Enter");
    at
}

#[test]
fn a_menu_row_types_the_command_as_the_persons_and_enters_after_the_echo() {
    let (mut app, wid, session, mut reader) = idle();
    frame(&mut app, wid);
    let human = human_seq(&app, session);
    app.open_fact_menu(wid, session, Fact::Effort, (0, 0), true);
    app.choose_autopilot_menu_row(menu_row(&app, "fact-menu/effort/type/key-on"));
    assert!(app.autopilot_menu.is_none(), "begun, the menu closes");
    assert_eq!(pty(&mut reader, 1), b"/", "the probe alone");
    assert_eq!(human_seq(&app, session), human + 1, "the person's write");
    assert_eq!(stage(&app, session), Some(Stage::Probed));
    assert_eq!(
        said(&app, wid, session).as_deref(),
        Some(format!("turning {KEY} on\u{2026}").as_str())
    );
    let at = Instant::now();
    app.advance_slash_presses(at);
    assert!(pty(&mut reader, 0).is_empty(), "nothing before the echo");
    echo(&app, session, CARET, "/");
    app.advance_slash_presses(at);
    let slash = Slash::TopEffort(true);
    assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
    assert_eq!(human_seq(&app, session), human + 1, "aterm's own write");
    echo(&app, session, CARET, &slash.text()[..9]);
    app.advance_slash_presses(at);
    assert!(pty(&mut reader, 0).is_empty());
    assert!(matches!(stage(&app, session), Some(Stage::Typed { .. })));
    echo(&app, session, CARET, &slash.text());
    app.advance_slash_presses(at);
    app.advance_slash_presses(at + QUIET / 2);
    assert!(pty(&mut reader, 0).is_empty(), "CONTROL: not quiet yet");
    let asked = crate::claude_slash::FOOTER_ASKED.with(std::cell::Cell::get);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(pty(&mut reader, 1), b"\r", "aterm's Enter");
    assert_eq!(human_seq(&app, session), human + 1);
    assert_eq!(stage(&app, session), Some(Stage::Entered));
    answered(&app, session, TOP, true);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(ended(&app, session), Some(SlashEnd::Done));
    assert_eq!(
        said(&app, wid, session).as_deref(),
        Some(format!("\u{2713} {KEY} on (this session)").as_str())
    );
    assert!(crate::claude_slash::FOOTER_ASKED.with(std::cell::Cell::get) >= asked + 2);
    assert!(pty(&mut reader, 0).is_empty(), "nothing after the answer");
    app.advance_slash_presses(Instant::now() + NOTICE_SHOWN);
    assert_eq!(ended(&app, session), None);
}

type Case = (&'static str, fn(&mut App, WindowId, u64), SlashRefusal);

#[test]
fn each_guard_types_nothing_and_names_why() {
    let cases: [Case; 14] = [
        (
            "a draft",
            |app, _, s| echo(app, s, CARET, "hello"),
            SlashRefusal::Draft,
        ),
        (
            "a turn in flight",
            |app, _, s| draw_claude(app, s, MANUAL_BUSY),
            SlashRefusal::Busy,
        ),
        (
            "a permission box",
            |app, _, s| feed(app, s, b"\x1b[2J\x1b[10;1H Do you want to proceed?"),
            SlashRefusal::Covered,
        ),
        (
            "the model picker",
            |app, wid, s| {
                draw_measured(
                    app,
                    wid,
                    s,
                    include_str!(
                        "../../aterm-phase/src/fixtures/claude-2.1.304-top-model-picker-120col.txt"
                    ),
                    |_| {},
                );
            },
            SlashRefusal::Covered,
        ),
        (
            "the effort slider",
            |app, wid, s| {
                draw_measured(
                    app,
                    wid,
                    s,
                    include_str!(
                        "../../aterm-phase/src/fixtures/claude-2.1.304-top-effort-slider-120col.txt"
                    ),
                    |_| {},
                );
            },
            SlashRefusal::Covered,
        ),
        (
            "the shell box",
            |app, wid, s| {
                draw_measured(
                    app,
                    wid,
                    s,
                    include_str!(
                        "../../aterm-phase/src/fixtures/claude-2.1.304-top-shell-mode-120col.txt"
                    ),
                    |_| {},
                );
            },
            SlashRefusal::Covered,
        ),
        (
            "a survey",
            |app, _, s| {
                feed(
                    app,
                    s,
                    "\x1b[18;1H\u{25CF} How is Claude doing this session? (optional)\
                     \x1b[19;1H\x1b[2K  1: Bad    2: Fine   3: Good   0: Dismiss\x1b[22;3H"
                        .as_bytes(),
                );
            },
            SlashRefusal::Survey,
        ),
        (
            "a scrolled view",
            |app, _, s| {
                let lines: String = (0..60).map(|i| format!("line {i}\r\n")).collect();
                feed(app, s, lines.as_bytes());
                draw_claude(app, s, MANUAL);
                let term = app.pool.get(s).expect("session").term.clone();
                crate::term_lock(&term).scroll_display(3);
            },
            SlashRefusal::Covered,
        ),
        (
            "a rewound view",
            |app, _, s| {
                assert_eq!(s, 0, "fixture: the recording helpers drive session 0");
                crate::rewind::tests::start_recording(app);
                let _ = crate::rewind::tests::slice(app, b"\x1b[0m");
                app.rewind_open(s, crate::rewind::Target::Latest, None)
                    .expect("open");
            },
            SlashRefusal::Covered,
        ),
        (
            "a halted session",
            |app, _, s| app.take_over_session(s).expect("the person halts it"),
            SlashRefusal::Halted,
        ),
        (
            "a parked session",
            |app, _, s| {
                app.pool
                    .get(s)
                    .expect("session")
                    .ctx
                    .update_parked
                    .park_for_test();
            },
            SlashRefusal::Parked,
        ),
        (
            "another driver's lease",
            |app, _, s| {
                *app.pool
                    .get(s)
                    .expect("session")
                    .ctx
                    .turn_lease
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(crate::Lease::Turn {
                        id: 7,
                        driver: Some(aterm_session::SessionId::new("another-driver")),
                        typing: false,
                    });
            },
            SlashRefusal::Driver,
        ),
        (
            "vim INSERT",
            |app, _, s| draw_claude(app, s, &format!("  -- INSERT -- {}", MANUAL.trim_start())),
            SlashRefusal::NotReady,
        ),
        (
            "a pane no Claude Code runs in",
            |app, _, s| {
                let entry = app.pool.get(s).expect("session");
                let mut tl = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                tl.set_agent_reader_for_test(false);
            },
            SlashRefusal::NotReady,
        ),
    ];
    for (name, make, why) in cases {
        let (mut app, wid, session, mut reader) = idle();
        make(&mut app, wid, session);
        let human = human_seq(&app, session);
        assert_eq!(
            app.press_slash(wid, session, Slash::TopEffort(true)),
            Err(why),
            "{name}"
        );
        assert!(pty(&mut reader, 0).is_empty(), "{name}: nothing typed");
        assert_eq!(human_seq(&app, session), human, "{name}");
        assert_eq!(
            said(&app, wid, session),
            Some(why.says("", None)),
            "{name}: said"
        );
        assert_eq!(stage(&app, session), None, "{name}");
        let (mut app, wid, session, mut reader) = idle();
        assert_eq!(
            app.press_slash(wid, session, Slash::TopEffort(true)),
            Ok(()),
            "{name}: control"
        );
        assert_eq!(pty(&mut reader, 1), b"/", "{name}: control");
    }
}

#[test]
fn no_enter_after_the_person_types() {
    for person in [true, false] {
        let (mut app, wid, session, mut reader) = idle();
        let slash = Slash::TopEffort(true);
        app.press_slash(wid, session, slash).expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        echo(&app, session, CARET, "/");
        let at = Instant::now();
        app.advance_slash_presses(at);
        assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
        echo(&app, session, CARET, &slash.text());
        app.advance_slash_presses(at);
        if person {
            app.note_person_input(
                session,
                &InputEvent::KeySequence(b"x".to_vec()),
                Source::Human,
            );
            echo(&app, session, CARET, &format!("{}x", slash.text()));
        }
        app.advance_slash_presses(at + QUIET);
        if person {
            assert!(pty(&mut reader, 0).is_empty(), "no Enter, no erase");
            assert_eq!(
                ended(&app, session),
                Some(SlashEnd::Stopped(SlashRefusal::Person))
            );
            assert_eq!(
                said(&app, wid, session).as_deref(),
                Some("you typed; nothing more sent")
            );
        } else {
            assert_eq!(pty(&mut reader, 1), b"\r", "CONTROL: the Enter goes");
        }
    }
}

#[test]
fn a_gesture_that_reaches_no_program_leaves_the_press_running() {
    for tracked in [false, true] {
        let (mut app, wid, session, mut reader) = idle();
        if tracked {
            feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        }
        frame(&mut app, wid);
        let slash = Slash::TopEffort(true);
        app.press_slash(wid, session, slash).expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        echo(&app, session, CARET, "/");
        let at = Instant::now();
        app.advance_slash_presses(at);
        assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
        echo(&app, session, CARET, &slash.text());
        app.advance_slash_presses(at);
        let human = human_seq(&app, session);
        let (x, y) = px_of(&app, wid, (footer_row(&app, wid) - 4, 4));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert_eq!(human_seq(&app, session), human + 1, "{tracked}: counted");
        let report = pty(&mut reader, usize::from(tracked));
        assert_eq!(
            report.is_empty(),
            !tracked,
            "{tracked}: reported: {report:?}"
        );
        app.advance_slash_presses(at + QUIET);
        app.advance_slash_presses(at + QUIET);
        if tracked {
            assert!(!pty(&mut reader, 0).contains(&b'\r'), "CONTROL: no Enter");
            assert_eq!(
                ended(&app, session),
                Some(SlashEnd::Stopped(SlashRefusal::Person))
            );
        } else {
            assert_eq!(pty(&mut reader, 1), b"\r", "the Enter goes");
            assert_eq!(stage(&app, session), Some(Stage::Entered));
        }
    }
}

#[test]
fn a_double_click_on_a_typed_menu_row_types_through_to_the_enter() {
    let slash = Slash::TopEffort(true);
    for late in [false, true] {
        let (mut app, wid, session, mut reader) = idle();
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        let anchor = app.windows[&wid]
            .claude_lights
            .fact_hits
            .iter()
            .find(|h| h.fact == Fact::Effort)
            .map(|h| (h.frame_row, h.cols.0))
            .expect("the effort on the glass");
        app.open_fact_menu(wid, session, Fact::Effort, anchor, false);
        frame(&mut app, wid);
        app.splice_autopilot_menu(wid);
        let i = menu_row(&app, "fact-menu/effort/type/key-on");
        let rect = app
            .autopilot_menu
            .as_ref()
            .and_then(|m| m.rect)
            .expect("painted");
        let (x, y) = px_of(&app, wid, (rect.row + 1 + i, rect.col + 3));
        app.on_cursor_moved(wid, x, y);
        // Each press states when it happened, so how long the probe's read takes never moves
        // the second across a double click's interval; a minute ago, so a press or a stamp that
        // read the clock instead lands on the wrong side of it.
        let first = Instant::now()
            .checked_sub(Duration::from_secs(60))
            .expect("a minute of uptime");
        app.on_mouse_input_at(wid, ElementState::Pressed, MouseButton::Left, first);
        app.on_mouse_input_at(wid, ElementState::Released, MouseButton::Left, first);
        assert!(app.autopilot_menu.is_none(), "chosen and closed");
        assert_eq!(pty(&mut reader, 1), b"/", "the probe alone");
        let interval = Duration::from_millis(u64::try_from(crate::multi_click_ms()).unwrap());
        let second = if late {
            first + interval + Duration::from_millis(1)
        } else {
            first + interval
        };
        app.on_mouse_input_at(wid, ElementState::Pressed, MouseButton::Left, second);
        app.on_mouse_input_at(wid, ElementState::Released, MouseButton::Left, second);
        echo(&app, session, CARET, "/");
        let at = Instant::now();
        app.advance_slash_presses(at);
        if late {
            let got = pty(&mut reader, 1);
            assert!(got.starts_with(b"\x1b[<0;"), "CONTROL: reported: {got:?}");
            assert!(!got.ends_with(&rest(slash)), "CONTROL: no rest");
            assert_eq!(
                ended(&app, session),
                Some(SlashEnd::Stopped(SlashRefusal::Person))
            );
            continue;
        }
        assert_eq!(
            pty(&mut reader, rest(slash).len()),
            rest(slash),
            "nothing reported"
        );
        echo(&app, session, CARET, &slash.text());
        app.advance_slash_presses(at);
        app.advance_slash_presses(at + QUIET);
        assert_eq!(pty(&mut reader, 1), b"\r", "aterm's Enter");
    }
}

#[test]
fn a_command_the_composer_wraps_gets_its_enter() {
    let slash = Slash::TopEffort(true);
    for whole in [true, false] {
        let (mut app, wid, session, mut reader) = idle();
        app.press_slash(wid, session, slash).expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        echo(&app, session, CARET, "/");
        let at = Instant::now();
        app.advance_slash_presses(at);
        assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
        let rule = "\u{2500}".repeat(cols(&app, session));
        let tail = if whole { "on" } else { "onx" };
        feed(
            &app,
            session,
            format!(
                "\x1b[20;1H\x1b[2K{rule}\x1b[21;1H\x1b[2K\u{276F} /effort {KEY}\
                 \x1b[22;1H\x1b[2K  {tail}\x1b[23;1H\x1b[2K{rule}\x1b[22;{}H",
                3 + tail.len()
            )
            .as_bytes(),
        );
        app.advance_slash_presses(at);
        app.advance_slash_presses(at + QUIET);
        if whole {
            assert_eq!(pty(&mut reader, 1), b"\r", "the Enter");
            assert_eq!(stage(&app, session), Some(Stage::Entered));
        } else {
            assert!(pty(&mut reader, 0).is_empty(), "CONTROL: no Enter");
        }
    }
}

#[test]
fn a_stop_after_the_probe_names_the_slash_it_left() {
    let survey = "\x1b[18;1H\u{25CF} How is Claude doing this session? (optional)\
                  \x1b[19;1H\x1b[2K  1: Bad    2: Fine   3: Good   0: Dismiss\x1b[22;4H";
    let cases: [(&str, &str, SlashRefusal, &str); 2] = [
        ("a survey", survey, SlashRefusal::Survey, "a survey is open"),
        (
            "a box",
            "\x1b[2J\x1b[10;1H Do you want to proceed?",
            SlashRefusal::Covered,
            "a dialog is open",
        ),
    ];
    for (name, drawn, why, head) in cases {
        let (mut app, wid, session, mut reader) = idle();
        app.press_slash(wid, session, Slash::TopEffort(true))
            .expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        echo(&app, session, CARET, "/");
        feed(&app, session, drawn.as_bytes());
        let at = Instant::now();
        app.advance_slash_presses(at);
        assert!(pty(&mut reader, 0).is_empty(), "{name}: no rest");
        app.advance_slash_presses(at + ECHO);
        assert_eq!(ended(&app, session), Some(SlashEnd::Stopped(why)), "{name}");
        assert_eq!(
            said(&app, wid, session),
            Some(format!("{head}; / is left in the composer")),
            "{name}"
        );
    }
    let (mut app, wid, session, mut reader) = idle();
    feed(&app, session, survey.replace("22;4H", "22;3H").as_bytes());
    assert_eq!(
        app.press_slash(wid, session, Slash::TopEffort(true)),
        Err(SlashRefusal::Survey)
    );
    assert!(pty(&mut reader, 0).is_empty());
    assert_eq!(
        said(&app, wid, session).as_deref(),
        Some("a survey is open; nothing typed")
    );
}

#[test]
fn a_hold_taken_mid_press_stops_it_before_the_enter() {
    type Hold = fn(&mut App, u64);
    let cases: [(&str, Option<Hold>, Option<SlashRefusal>); 4] = [
        (
            "another driver's lease",
            Some(|app, s| {
                *app.pool
                    .get(s)
                    .expect("session")
                    .ctx
                    .turn_lease
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(crate::Lease::Turn {
                        id: 7,
                        driver: Some(aterm_session::SessionId::new("another-driver")),
                        typing: false,
                    });
            }),
            Some(SlashRefusal::Driver),
        ),
        (
            "a halt",
            Some(|app, s| app.take_over_session(s).expect("the person halts it")),
            Some(SlashRefusal::Halted),
        ),
        (
            "a park",
            Some(|app, s| {
                app.pool
                    .get(s)
                    .expect("session")
                    .ctx
                    .update_parked
                    .park_for_test();
            }),
            Some(SlashRefusal::Parked),
        ),
        ("no hold", None, None),
    ];
    let slash = Slash::TopEffort(true);
    for (name, hold, why) in cases {
        let (mut app, wid, session, mut reader) = idle();
        app.press_slash(wid, session, slash).expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        echo(&app, session, CARET, "/");
        let at = Instant::now();
        app.advance_slash_presses(at);
        assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
        echo(&app, session, CARET, &slash.text());
        app.advance_slash_presses(at);
        if let Some(hold) = hold {
            hold(&mut app, session);
        }
        app.advance_slash_presses(at + QUIET);
        match why {
            Some(why) => {
                assert!(pty(&mut reader, 0).is_empty(), "{name}: no Enter");
                assert_eq!(ended(&app, session), Some(SlashEnd::Stopped(why)), "{name}");
                assert_eq!(
                    said(&app, wid, session),
                    Some(why.says(&slash.text(), None)),
                    "{name}"
                );
            }
            None => assert_eq!(pty(&mut reader, 1), b"\r", "CONTROL: the Enter"),
        }
    }

    use crate::session_timeline::SessionTimeline;

    fn image(start: u64) -> std::sync::Arc<crate::holder_image::HolderImage> {
        std::sync::Arc::new(crate::holder_image::HolderImage::of(
            PGID,
            Some(start),
            Some(std::path::PathBuf::from("/fixture/claude")),
            atpkg::image::ImageBuild {
                program: atpkg::image::ProgramTag::new("claude"),
                build: 0,
                source: atpkg::image::ImageSource::Native,
            },
            0,
        ))
    }

    type Change = fn(&mut SessionTimeline);
    let changes: [(&str, Change, bool); 7] = [
        (
            "another group",
            |tl| {
                tl.note_foreground_group(PGID + 1);
                tl.set_agent_reader_for_test(true);
            },
            true,
        ),
        (
            "group away and back",
            |tl| {
                tl.note_foreground_group(PGID + 1);
                tl.note_foreground_group(PGID);
                tl.set_program(PGID, Some("node".into()));
                tl.set_agent_reader_for_test(true);
            },
            true,
        ),
        (
            "same-group program away and back",
            |tl| {
                tl.set_program(PGID, Some("zsh".into()));
                tl.set_program(PGID, Some("node".into()));
            },
            true,
        ),
        (
            "runtime reader away and back",
            |tl| {
                tl.set_agent_reader_for_test(false);
                tl.set_agent_reader_for_test(true);
            },
            true,
        ),
        (
            "same-group image replaced",
            |tl| {
                tl.offer_holder_image(image(2), crate::holder_image::EdgeKind::Exec);
            },
            true,
        ),
        (
            "unknown group",
            |tl| {
                tl.note_foreground_group(0);
            },
            true,
        ),
        ("same identity", |_| {}, false),
    ];
    for after_text in [false, true] {
        for (name, change, stopped) in changes {
            let (mut app, wid, session, mut reader) = idle();
            let timeline = app.pool.get(session).expect("session").ctx.timeline.clone();
            {
                let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
                tl.set_program(PGID, Some("node".into()));
                tl.offer_holder_image(image(1), crate::holder_image::EdgeKind::Exec);
            }
            let held_timeline = timeline.clone();
            let wrote_under_custody =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let witnessed = wrote_under_custody.clone();
            app.pool
                .get(session)
                .expect("session")
                .ctx
                .sink
                .install_input_hook(move || {
                    assert!(
                        matches!(
                            held_timeline.try_lock(),
                            Err(std::sync::TryLockError::WouldBlock)
                        ),
                        "published identity is held through the actual sink write"
                    );
                    witnessed.store(true, std::sync::atomic::Ordering::SeqCst);
                });
            app.press_slash(wid, session, slash).expect("begun");
            assert_eq!(pty(&mut reader, 1), b"/");
            assert!(wrote_under_custody.load(std::sync::atomic::Ordering::SeqCst));
            echo(&app, session, CARET, "/");
            let at = Instant::now();
            if after_text {
                app.advance_slash_presses(at);
                assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
                echo(&app, session, CARET, &slash.text());
                app.advance_slash_presses(at);
            }
            let read = app.slash_engine_read(session).expect("live engine");
            let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
            let stamp = crate::session_timeline::AgentStamp {
                generation: read.generation,
                fp: 0,
            };
            tl.publish_agent(
                "busy",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                stamp,
            );
            tl.publish_agent(
                "idle",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                stamp,
            );
            change(&mut tl);
            drop(tl);
            app.advance_slash_presses(at + QUIET);
            if stopped {
                assert!(
                    pty(&mut reader, 0).is_empty(),
                    "{name}, after_text={after_text}: no continuation"
                );
                assert_eq!(
                    ended(&app, session),
                    Some(SlashEnd::Stopped(SlashRefusal::Changed)),
                    "{name}"
                );
            } else if after_text {
                assert_eq!(pty(&mut reader, 1), b"\r", "same process: Enter");
            } else {
                assert_eq!(
                    pty(&mut reader, rest(slash).len()),
                    rest(slash),
                    "same process: rest"
                );
            }
        }

        let (mut app, wid, session, mut reader) = idle();
        app.press_slash(wid, session, slash).expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        echo(&app, session, CARET, "/");
        let at = Instant::now();
        if after_text {
            app.advance_slash_presses(at);
            assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
            echo(&app, session, CARET, &slash.text());
            app.advance_slash_presses(at);
        }
        let timeline = app.pool.get(session).expect("session").ctx.timeline.clone();
        crate::claude_slash::BETWEEN_READ_AND_WRITE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .note_foreground_group(PGID + 1);
            }));
        });
        app.advance_slash_presses(at + QUIET);
        crate::claude_slash::BETWEEN_READ_AND_WRITE.with(|hook| *hook.borrow_mut() = None);
        assert!(
            pty(&mut reader, 0).is_empty(),
            "replaced between read/write: no byte"
        );
        app.advance_slash_presses(at + QUIET);
        assert_eq!(
            ended(&app, session),
            Some(SlashEnd::Stopped(SlashRefusal::Changed))
        );
    }
}

#[test]
fn a_press_arms_one_wake_at_a_time() {
    let (mut app, wid, session, mut reader) = idle();
    crate::work_counts::take_footer_wakes_armed();
    app.press_slash(wid, session, Slash::TopEffort(true))
        .expect("begun");
    assert_eq!(
        crate::work_counts::take_footer_wakes_armed(),
        1,
        "the poll alone"
    );
    assert_eq!(pty(&mut reader, 1), b"/");
    echo(&app, session, CARET, "/");
    let at = Instant::now();
    app.advance_slash_presses(at);
    let slash = Slash::TopEffort(true);
    assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
    echo(&app, session, CARET, &slash.text());
    app.advance_slash_presses(at);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(pty(&mut reader, 1), b"\r");
    answered(&app, session, TOP, true);
    crate::work_counts::take_footer_wakes_armed();
    app.advance_slash_presses(at + QUIET);
    assert_eq!(ended(&app, session), Some(SlashEnd::Done));
    assert_eq!(
        crate::work_counts::take_footer_wakes_armed(),
        1,
        "one wake for the notice's end"
    );
    let (mut app, wid, session, mut reader) = idle();
    app.press_slash(wid, session, Slash::ModelPicker)
        .expect("begun");
    assert_eq!(pty(&mut reader, 1), b"/");
    echo(&app, session, CARET, "/");
    let at = Instant::now();
    app.advance_slash_presses(at);
    assert_eq!(pty(&mut reader, 5), b"model");
    feed(
        &app,
        session,
        "\x1b[24;1H\x1b[2K  \u{276F} /model   Set the AI model".as_bytes(),
    );
    echo(&app, session, CARET, "/model");
    app.advance_slash_presses(at);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(pty(&mut reader, 1), b"\r");
    draw_measured(
        &mut app,
        wid,
        session,
        include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-model-picker-120col.txt"),
        |_| {},
    );
    crate::work_counts::take_footer_wakes_armed();
    app.advance_slash_presses(at + QUIET);
    assert_eq!(ended(&app, session), Some(SlashEnd::Done));
    assert_eq!(crate::work_counts::take_footer_wakes_armed(), 1, "CONTROL");
}

#[test]
fn a_screen_that_moves_between_read_and_write_writes_nothing() {
    for (bytes, typed) in [
        (&b"\x1b[2J\x1b[10;1H Do you want to proceed?"[..], false),
        (&b"\x1b[18;1H\xe2\x97\x8f more\x1b[22;3H"[..], true),
        (&b""[..], true),
    ] {
        let (mut app, wid, session, mut reader) = idle();
        let term = app.pool.get(session).expect("session").term.clone();
        let mut once = Some(bytes.to_vec());
        crate::claude_slash::BETWEEN_READ_AND_WRITE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                if let Some(bytes) = once.take()
                    && !bytes.is_empty()
                {
                    crate::term_lock(&term).process(&bytes);
                }
            }));
        });
        let result = app.press_slash(wid, session, Slash::TopEffort(true));
        crate::claude_slash::BETWEEN_READ_AND_WRITE.with(|hook| *hook.borrow_mut() = None);
        if typed {
            assert_eq!(result, Ok(()), "{bytes:?}");
            assert_eq!(pty(&mut reader, 1), b"/", "{bytes:?}");
        } else {
            assert_eq!(result, Err(SlashRefusal::Covered));
            assert!(pty(&mut reader, 0).is_empty(), "nothing into the box");
        }
    }
    for back in [false, true] {
        let (mut app, wid, session, mut reader) = idle();
        let timeline = app.pool.get(session).expect("session").ctx.timeline.clone();
        let mut once = true;
        crate::claude_slash::BETWEEN_READ_AND_WRITE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                if once {
                    once = false;
                    let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
                    tl.note_foreground_group(PGID + 1);
                    if back {
                        tl.note_foreground_group(PGID);
                        tl.set_agent_reader_for_test(true);
                    }
                }
            }));
        });
        let result = app.press_slash(wid, session, Slash::TopEffort(true));
        crate::claude_slash::BETWEEN_READ_AND_WRITE.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(result, Err(SlashRefusal::Moved));
        assert!(
            pty(&mut reader, 0).is_empty(),
            "a replaced target gets no probe, back={back}"
        );
    }
}

const HALF_FRAME: &str = "\x1b[?2026h\x1b[H\x1b[2J\u{23FA} working";

fn half_frame(app: &App, session: u64) {
    feed(app, session, HALF_FRAME.as_bytes());
    let term = app.pool.get(session).expect("session").term.clone();
    assert!(
        crate::term_lock(&term).frame_unfinished_at(Instant::now()),
        "precondition: the frame is unfinished"
    );
}

fn rest_of_frame(app: &App, session: u64, typed: &str) {
    draw_claude(app, session, MANUAL);
    if !typed.is_empty() {
        echo(app, session, CARET, typed);
    }
    feed(app, session, b"\x1b[?2026l");
}

#[test]
fn a_click_on_half_a_frame_is_refused_as_claude_drawing() {
    let (mut app, wid, session, mut reader) = idle();
    half_frame(&app, session);
    assert_eq!(
        app.press_slash(wid, session, Slash::TopEffort(true)),
        Err(SlashRefusal::Moved)
    );
    assert!(
        pty(&mut reader, 0).is_empty(),
        "nothing typed on half a frame"
    );
    assert_eq!(
        said(&app, wid, session),
        Some(SlashRefusal::Moved.says("", None))
    );
    rest_of_frame(&app, session, "");
    app.press_slash(wid, session, Slash::TopEffort(true))
        .expect("the whole frame's click types");
    assert_eq!(pty(&mut reader, 1), b"/");
}

#[test]
fn a_typed_press_waits_through_half_a_frame() {
    let (mut app, wid, session, mut reader) = idle();
    let slash = Slash::TopEffort(true);
    app.press_slash(wid, session, slash).expect("begun");
    assert_eq!(pty(&mut reader, 1), b"/");
    echo(&app, session, CARET, "/");
    let at = Instant::now();
    app.advance_slash_presses(at);
    assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
    half_frame(&app, session);
    app.advance_slash_presses(at);
    assert_eq!(ended(&app, session), None, "no stop on half a frame");
    assert!(matches!(stage(&app, session), Some(Stage::Typed { .. })));
    assert!(pty(&mut reader, 0).is_empty(), "nothing written");
    rest_of_frame(&app, session, &slash.text());
    app.advance_slash_presses(at);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(pty(&mut reader, 1), b"\r", "the whole frame's Enter");
}

#[test]
fn an_entered_picker_press_is_not_done_on_half_a_frame() {
    let (mut app, wid, session, mut reader) = idle();
    app.press_slash(wid, session, Slash::ModelPicker)
        .expect("begun");
    assert_eq!(pty(&mut reader, 1), b"/");
    echo(&app, session, CARET, "/");
    let at = Instant::now();
    app.advance_slash_presses(at);
    assert_eq!(pty(&mut reader, 5), b"model");
    feed(
        &app,
        session,
        "\x1b[24;1H\x1b[2K  \u{276F} /model   Set the AI model".as_bytes(),
    );
    echo(&app, session, CARET, "/model");
    app.advance_slash_presses(at);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(pty(&mut reader, 1), b"\r");
    assert_eq!(stage(&app, session), Some(Stage::Entered));
    half_frame(&app, session);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(ended(&app, session), None, "not Done on half a frame");
    assert_eq!(stage(&app, session), Some(Stage::Entered));
    rest_of_frame(&app, session, "");
    app.advance_slash_presses(at + QUIET);
    assert_eq!(
        stage(&app, session),
        Some(Stage::Entered),
        "still awaiting the answer"
    );
    draw_measured(
        &mut app,
        wid,
        session,
        include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-model-picker-120col.txt"),
        |_| {},
    );
    app.advance_slash_presses(at + QUIET);
    assert_eq!(ended(&app, session), Some(SlashEnd::Done));
}

#[test]
fn the_footer_press_never_waits_for_a_frame_on_the_event_loop() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (mut app, wid, session, mut reader) = idle();
    let (term, watchers) = {
        let entry = app.pool.get(session).expect("session");
        (entry.term.clone(), entry.ctx.watchers.clone())
    };
    watchers.bind_frame(&term);
    crate::term_lock(&term).apply_config(&aterm_core::config::TerminalConfig {
        sync_timeout_ms: 60_000,
        ..Default::default()
    });
    let hook_term = term.clone();
    let mut once = true;
    crate::claude_slash::BETWEEN_READ_AND_WRITE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            if std::mem::take(&mut once) {
                crate::term_lock(&hook_term).process(HALF_FRAME.as_bytes());
            }
        }));
    });
    let done = AtomicBool::new(false);
    let (result, waited) = std::thread::scope(|s| {
        let watching = s.spawn(|| {
            let waited = aterm_spec::wait::until_within(
                Duration::from_secs(60),
                "the press answers, or waits",
                || {
                    if watchers.count() > 0 {
                        Some(true)
                    } else {
                        done.load(Ordering::Acquire).then_some(false)
                    }
                },
            );
            if waited {
                let mut t = crate::term_lock(&term);
                t.process(b"\x1b[?2026l");
                drop(t);
                watchers.ring_watchers();
            }
            waited
        });
        let result = app.press_slash(wid, session, Slash::TopEffort(true));
        done.store(true, Ordering::Release);
        (result, watching.join().expect("the watcher"))
    });
    crate::claude_slash::BETWEEN_READ_AND_WRITE.with(|hook| *hook.borrow_mut() = None);
    assert!(!waited, "the event loop waited for Claude's frame");
    assert_eq!(result, Err(SlashRefusal::Moved));
    assert!(pty(&mut reader, 0).is_empty(), "nothing written");
}

#[test]
fn one_aterm_input_per_session() {
    let (mut app, wid, session, mut reader) = idle();
    app.toggle_claude_light(wid, session, Light::Mode);
    assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
    assert_eq!(
        app.press_slash(wid, session, Slash::ModelPicker),
        Err(SlashRefusal::InFlight)
    );
    assert!(pty(&mut reader, 0).is_empty());
    feed(
        &app,
        session,
        format!("\x1b[24;1H\x1b[2K{BYPASS}\x1b[22;3H").as_bytes(),
    );
    app.on_claude_footer_changed(session);
    assert!(pty(&mut reader, 0).is_empty());
    assert_eq!(app.press_slash(wid, session, Slash::ModelPicker), Ok(()));
    assert_eq!(pty(&mut reader, 1), b"/");

    let (mut app, wid, session, mut reader) = idle();
    app.press_slash(wid, session, Slash::ModelPicker)
        .expect("begun");
    assert_eq!(pty(&mut reader, 1), b"/");
    app.toggle_claude_light(wid, session, Light::Mode);
    assert!(pty(&mut reader, 0).is_empty(), "no shift+tab beside it");
    assert_eq!(
        app.windows[&wid].claude_lights.sessions[&session]
            .refused
            .map(|(_, why, _)| why),
        Some(super::Refusal::Typing)
    );
    app.note_person_input(
        session,
        &InputEvent::KeySequence(b"x".to_vec()),
        Source::Human,
    );
    app.advance_slash_presses(Instant::now());
    assert_eq!(
        ended(&app, session),
        Some(SlashEnd::Stopped(SlashRefusal::Person))
    );
    app.toggle_claude_light(wid, session, Light::Mode);
    assert_eq!(pty(&mut reader, 3), lights::SHIFT_TAB);
}

/// A light pressed on a rewound pane is refused and begins no toggle, since its press could not reach the program.
#[test]
fn a_light_on_a_rewound_pane_is_refused_and_begins_no_toggle() {
    let (mut app, wid, session, mut reader) = idle();
    assert_eq!(session, 0, "fixture: the recording helpers drive session 0");
    crate::rewind::tests::start_recording(&app);
    let _ = crate::rewind::tests::slice(&app, b"\x1b[0m");
    app.rewind_open(session, crate::rewind::Target::Latest, None)
        .expect("open");
    app.toggle_claude_light(wid, session, Light::Mode);
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    let s = &app.windows[&wid].claude_lights.sessions[&session];
    assert!(s.pending.is_none(), "no toggle began");
    assert!(
        matches!(s.refused, Some((Light::Mode, super::Refusal::NotShown, _))),
        "refused as not on screen"
    );
    assert!(app.rewind_close(session));
    app.toggle_claude_light(wid, session, Light::Mode);
    assert_eq!(
        pty(&mut reader, 3),
        lights::SHIFT_TAB,
        "CONTROL: live, one press"
    );
}

#[test]
fn an_echo_that_differs_gets_no_enter_and_is_left() {
    let slash = Slash::TopEffort(false);
    let (mut app, wid, session, mut reader) = idle();
    app.press_slash(wid, session, slash).expect("begun");
    assert_eq!(pty(&mut reader, 1), b"/");
    echo(&app, session, CARET, "x/");
    app.advance_slash_presses(Instant::now());
    assert!(pty(&mut reader, 0).is_empty(), "no rest");
    assert_eq!(
        ended(&app, session),
        Some(SlashEnd::Stopped(SlashRefusal::Changed))
    );
    let (mut app, wid, session, mut reader) = idle();
    app.press_slash(wid, session, slash).expect("begun");
    assert_eq!(pty(&mut reader, 1), b"/");
    echo(&app, session, CARET, "/");
    let at = Instant::now();
    app.advance_slash_presses(at);
    assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
    let other = format!("{}x", slash.text());
    echo(&app, session, CARET, &other);
    app.advance_slash_presses(at);
    app.advance_slash_presses(at + QUIET);
    assert!(pty(&mut reader, 0).is_empty(), "no Enter, no erase");
    assert_eq!(
        ended(&app, session),
        Some(SlashEnd::Stopped(SlashRefusal::LeftTyped))
    );
    assert_eq!(
        said(&app, wid, session),
        Some(format!("{other} is in the composer; press Enter to run it"))
    );
}

#[test]
fn an_open_picker_ends_the_press_and_aterm_presses_nothing_more() {
    for pointed in ["/model", "/code-review"] {
        let (mut app, wid, session, mut reader) = idle();
        frame(&mut app, wid);
        app.open_fact_menu(wid, session, Fact::Model, (0, 0), true);
        app.choose_autopilot_menu_row(menu_row(&app, "fact-menu/model/type/model"));
        assert_eq!(pty(&mut reader, 1), b"/");
        echo(&app, session, CARET, "/");
        let at = Instant::now();
        app.advance_slash_presses(at);
        assert_eq!(pty(&mut reader, 5), b"model");
        feed(
            &app,
            session,
            format!(
                "\x1b[18;1H\x1b[2K  \u{276F} {pointed}   Set the AI model\
                 \x1b[19;1H\x1b[2K    /update-config   Configure\
                 \x1b[20;1H\x1b[2K    /code-review   Review"
            )
            .as_bytes(),
        );
        echo(&app, session, CARET, "/model");
        app.advance_slash_presses(at);
        app.advance_slash_presses(at + QUIET);
        if pointed != "/model" {
            assert!(pty(&mut reader, 0).is_empty(), "CONTROL: no Enter");
            app.advance_slash_presses(at + ECHO);
            assert!(pty(&mut reader, 0).is_empty());
            assert_eq!(
                ended(&app, session),
                Some(SlashEnd::Stopped(SlashRefusal::LeftTyped))
            );
            continue;
        }
        assert_eq!(pty(&mut reader, 1), b"\r");
        draw_measured(
            &mut app,
            wid,
            session,
            include_str!(
                "../../aterm-phase/src/fixtures/claude-2.1.304-top-model-picker-120col.txt"
            ),
            |_| {},
        );
        app.advance_slash_presses(at + QUIET);
        assert_eq!(ended(&app, session), Some(SlashEnd::Done));
        assert_eq!(said(&app, wid, session), None, "the picker says itself");
        for later in [QUIET, ECHO, REFUSAL_SHOWN * 2] {
            app.advance_slash_presses(at + later);
            app.on_claude_footer_changed(session);
        }
        assert!(pty(&mut reader, 0).is_empty(), "nothing into the picker");
    }
}

#[test]
fn a_background_agents_wait_is_no_live_turn() {
    const PANEL: &str =
        include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-agents-panel-120col.txt");
    for live in [false, true] {
        let (mut app, wid, session, mut reader) = rig();
        draw_measured(&mut app, wid, session, PANEL, |rows| {
            if live {
                for row in rows.iter_mut() {
                    if row.starts_with("\u{273B} Waiting for") {
                        *row = "\u{273B} Cogitating\u{2026} (3s \u{00B7} esc to interrupt)".into();
                    }
                }
            }
        })
        .expect("a caret");
        let screen = app.claude_screen_now(session).expect("the composer");
        assert!(screen.busy, "fixture guard: the wait reads busy");
        let got = app.press_slash(wid, session, Slash::TopEffort(false));
        if live {
            assert_eq!(got, Err(SlashRefusal::Busy));
            assert!(pty(&mut reader, 0).is_empty());
        } else {
            assert_eq!(got, Ok(()));
            assert_eq!(pty(&mut reader, 1), b"/");
        }
    }
}

fn mode_row_after_probe(app: &App, session: u64, mode: &str) {
    feed(
        app,
        session,
        format!("\x1b[24;1H\x1b[2K{mode}\x1b[22;4H").as_bytes(),
    );
}

#[test]
fn a_turn_that_goes_live_after_the_probe_holds_the_rest() {
    let slash = Slash::TopEffort(true);
    for (live, ends) in [(false, false), (true, true), (true, false)] {
        let (mut app, wid, session, mut reader) = idle();
        app.press_slash(wid, session, slash).expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        echo(&app, session, CARET, "/");
        if live {
            mode_row_after_probe(&app, session, MANUAL_BUSY);
        }
        let at = Instant::now();
        app.advance_slash_presses(at);
        if !live {
            assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash), "CONTROL");
            continue;
        }
        assert!(pty(&mut reader, 0).is_empty(), "nothing mid-turn");
        assert_eq!(stage(&app, session), Some(Stage::Probed));
        if ends {
            mode_row_after_probe(&app, session, MANUAL);
            app.advance_slash_presses(at);
            assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash), "it ended");
            continue;
        }
        app.advance_slash_presses(at + ECHO);
        assert!(pty(&mut reader, 0).is_empty(), "nothing, ever");
        assert_eq!(
            ended(&app, session),
            Some(SlashEnd::Stopped(SlashRefusal::Busy))
        );
        assert_eq!(
            said(&app, wid, session),
            Some(SlashRefusal::Busy.says("/", None))
        );
    }
}

#[test]
fn the_slash_menu_is_read_on_either_side_of_the_composer() {
    use crate::claude_slash::slash_menu;
    const INLINE: &str = include_str!(
        "../../aterm-phase/src/fixtures/claude-2.1.304-slash-menu-inline-model-120col.txt"
    );
    const PILL: &str =
        include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-mode-pill-auto-120col.txt");
    let screen = |text: &str| -> Vec<String> { text.lines().skip(1).map(str::to_owned).collect() };
    let menu = |rows: &[String]| {
        let (top, bottom) = aterm_phase::phase::composer_rules(rows).expect("a composer");
        slash_menu(rows, top, bottom)
    };
    let inline = screen(INLINE);
    assert_eq!(menu(&inline), Some(Some("/model".to_owned())), "under");
    let (top, bottom) = aterm_phase::phase::composer_rules(&inline).expect("a composer");
    let entries: Vec<String> = inline[bottom + 1..bottom + 4].to_vec();
    let mut over = vec![String::new(); 4];
    over.extend(entries.iter().cloned());
    over.extend(inline[top..=bottom].iter().cloned());
    over.push("  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)".into());
    assert_eq!(menu(&over), Some(Some("/model".to_owned())), "over");
    let moved: Vec<String> = inline
        .iter()
        .map(|r| {
            if r.starts_with("  \u{276F} /model") {
                r.replacen("  \u{276F} /model", "    /model", 1)
            } else if r.starts_with("    /code-review") {
                r.replacen("    /code-review", "  \u{276F} /code-review", 1)
            } else {
                r.clone()
            }
        })
        .collect();
    assert_eq!(menu(&moved), Some(Some("/code-review".to_owned())));
    let two: Vec<String> = inline
        .iter()
        .map(|r| r.replacen("    /code-review", "  \u{276F} /code-review", 1))
        .collect();
    assert_eq!(menu(&two), Some(None), "two pointers: none");
    assert_eq!(menu(&screen(PILL)), None, "the mode row: no menu");
}

#[test]
fn a_bare_commands_enter_waits_for_the_menu_under_the_composer() {
    for pointed in ["/model", "/code-review"] {
        let (mut app, wid, session, mut reader) = idle();
        app.press_slash(wid, session, Slash::ModelPicker)
            .expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        echo(&app, session, CARET, "/");
        let at = Instant::now();
        app.advance_slash_presses(at);
        assert_eq!(pty(&mut reader, 5), b"model");
        feed(
            &app,
            session,
            format!("\x1b[24;1H\x1b[2K  \u{276F} {pointed}   Set the AI model").as_bytes(),
        );
        echo(&app, session, CARET, "/model");
        app.advance_slash_presses(at);
        app.advance_slash_presses(at + QUIET);
        if pointed == "/model" {
            assert_eq!(pty(&mut reader, 1), b"\r", "its own entry: the Enter");
            continue;
        }
        assert!(pty(&mut reader, 0).is_empty(), "CONTROL: no Enter");
        app.advance_slash_presses(at + ECHO);
        assert!(pty(&mut reader, 0).is_empty());
        assert_eq!(
            ended(&app, session),
            Some(SlashEnd::Stopped(SlashRefusal::LeftTyped))
        );
    }
}

#[test]
fn vim_normal_takes_the_probe_as_nothing_and_the_press_stops_untouched() {
    const NORMAL: &str =
        include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-vim-normal-120col.txt");
    for echoes in [false, true] {
        let (mut app, wid, session, mut reader) = rig();
        let caret = draw_measured(&mut app, wid, session, NORMAL, |_| {}).expect("a caret");
        app.press_slash(wid, session, Slash::TopEffort(true))
            .expect("the probe goes: nothing tells NORMAL apart");
        assert_eq!(pty(&mut reader, 1), b"/");
        if echoes {
            echo(&app, session, caret, "/");
        }
        let at = Instant::now();
        app.advance_slash_presses(at);
        if echoes {
            let slash = Slash::TopEffort(true);
            assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash), "CONTROL");
            continue;
        }
        assert!(pty(&mut reader, 0).is_empty(), "no motion");
        app.advance_slash_presses(at + ECHO);
        assert!(pty(&mut reader, 0).is_empty(), "no motion, ever");
        assert_eq!(
            ended(&app, session),
            Some(SlashEnd::Stopped(SlashRefusal::Vim))
        );
        assert_eq!(
            said(&app, wid, session).as_deref(),
            Some("the composer took no text (vim normal mode); nothing more sent")
        );
    }
}

#[test]
fn vim_insert_is_refused_by_its_own_check_and_normal_is_not() {
    for (name, text) in [
        (
            "insert",
            include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-vim-insert-120col.txt"),
        ),
        (
            "insert after normal's motions",
            include_str!(
                "../../aterm-phase/src/fixtures/claude-2.1.304-top-vim-normal-typed-120col.txt"
            ),
        ),
        (
            "insert under the refusal",
            include_str!(
                "../../aterm-phase/src/fixtures/claude-2.1.304-top-refused-needs-workflows-vim-insert-120col.txt"
            ),
        ),
        (
            "normal",
            include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-vim-normal-120col.txt"),
        ),
    ] {
        let insert = name != "normal";
        let (mut app, wid, session, mut reader) = rig();
        draw_measured(&mut app, wid, session, text, |_| {});
        let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
        let screen = lights::read_screen(&rows).expect(name);
        assert_eq!(
            (screen.mode, screen.vim_insert),
            (Some(lights::Mode::Auto), insert),
            "{name}: the lights read its pill"
        );
        let human = human_seq(&app, session);
        let got = app.press_slash(wid, session, Slash::TopEffort(true));
        if insert {
            assert_eq!(got, Err(SlashRefusal::NotReady), "{name}");
            assert!(pty(&mut reader, 0).is_empty(), "{name}: nothing typed");
            assert_eq!(human_seq(&app, session), human, "{name}");
        } else {
            assert_eq!(got, Ok(()), "CONTROL: {name}");
            assert_eq!(pty(&mut reader, 1), b"/", "CONTROL: the probe");
        }
    }
}

#[test]
fn claudes_refusal_latches_the_key_row_for_that_model() {
    for (answer, why, says) in [
        (
            "isn't available on claude-sonnet-4-5. Valid options are: low, medium, high",
            SlashRefusal::Unavailable,
            format!("{} isn't available on claude-sonnet-4-5", key_capitalised()),
        ),
        (
            "needs dynamic workflows enabled (see /config). Valid options are: low",
            SlashRefusal::NeedsWorkflows,
            format!(
                "{} needs dynamic workflows (see /config)",
                key_capitalised()
            ),
        ),
    ] {
        let (mut app, wid, session, mut reader) = idle();
        let at = through_enter(&mut app, wid, session, &mut reader, Slash::TopEffort(true));
        feed(
            &app,
            session,
            format!(
                "\x1b[17;1H\x1b[2K\u{276F} {}\x1b[18;1H\x1b[2K  \u{23BF}  {} {answer}\
                 \x1b[19;1H\x1b[2K\x1b[20;1H\x1b[2K",
                Slash::TopEffort(true).text(),
                key_capitalised()
            )
            .as_bytes(),
        );
        answered(&app, session, TOP, false);
        app.advance_slash_presses(at + QUIET);
        assert_eq!(ended(&app, session), Some(SlashEnd::Stopped(why)));
        assert_eq!(said(&app, wid, session), Some(says.clone()));
        app.open_fact_menu(wid, session, Fact::Effort, (0, 0), true);
        assert!(!menu_offers(&app, "/type/key-"), "{why:?}: latched");
        assert!(menu_offers(&app, "/type/effort"), "the slider stays");
        claude_on(&app, session, "Sonnet 5.5");
        app.open_fact_menu(wid, session, Fact::Effort, (0, 0), true);
        assert!(menu_offers(&app, "/type/key-on"), "{why:?}: a new model");
    }
}

#[test]
fn a_refused_menu_row_keeps_its_menu_under_a_note() {
    let (mut app, wid, session, mut reader) = idle();
    echo(&app, session, CARET, "draft");
    app.open_fact_menu(wid, session, Fact::Model, (0, 0), true);
    app.choose_autopilot_menu_row(menu_row(&app, "fact-menu/model/type/model"));
    assert!(pty(&mut reader, 0).is_empty());
    assert!(app.autopilot_menu.is_some(), "the menu stays");
    assert!(
        menu_notes(&app)
            .iter()
            .any(|n| n == "the composer holds a draft; nothing typed")
    );
    let menu = app.autopilot_menu.as_ref().expect("open");
    let row = &menu.rows[menu_row(&app, "fact-menu/model/type/model")];
    assert!(matches!(row, MenuRow::Item { enabled: false, .. }));
    app.open_fact_menu(wid, session, Fact::Model, (0, 0), true);
    assert!(!menu_notes(&app).iter().any(|n| n.contains("nothing typed")));
}

/// The slash press's model state, read off the genuine App: the stage, the writes, the person clock against the press's own, and how it ended from the session's facts and the window's title slot; the composer, its echo, a box and a live turn from the ENGINE.
pub(super) fn project_slash_press(
    app: &App,
    wid: WindowId,
    session: u64,
    expected: &aterm_spec::interp::State,
) -> aterm_spec::interp::State {
    let mut observed = expected.clone();
    let facts = app.claude_facts.get(&session);
    let press = facts.and_then(|f| f.press.as_ref());
    let now = Instant::now();
    let phase = match press.map(|p| p.stage) {
        Some(Stage::Probed) => 1,
        Some(Stage::Typed { .. }) => 2,
        Some(Stage::Entered) => 3,
        None if facts
            .and_then(|f| f.ended.as_ref())
            .is_some_and(|(_, _, until)| *until > now) =>
        {
            4
        }
        None => 0,
    };
    observed.insert("phase", phase);
    if let Some(p) = press {
        observed.insert("writes", i64::from(p.writes));
        let reached = app
            .pool
            .get(session)
            .expect("session")
            .ctx
            .human_input
            .counts()
            .reached;
        observed.insert("person", i64::from(reached > p.reached_after));
    }
    if phase == 4 {
        let end = facts.and_then(|f| f.ended.as_ref()).map(|(_, end, _)| *end);
        let words = app.windows[&wid]
            .claude_lights
            .sessions
            .get(&session)
            .and_then(|s| s.fact_notice.as_ref())
            .is_some_and(|(n, until)| matches!(n, FactNotice::Said(_)) && *until > now);
        observed.insert("named", i64::from(end == Some(SlashEnd::Done) || words));
    }
    let read = app.slash_engine_read(session);
    let screen = read.as_ref().and_then(|r| lights::read_screen(&r.rows));
    observed.insert("boxed", i64::from(screen.is_none()));
    if let (Some(r), Some(screen)) = (&read, &screen) {
        observed.insert(
            "busy",
            i64::from(screen.busy && !aterm_phase::phase::background_only(&r.rows)),
        );
    }
    if let (Some(r), Some(p)) = (&read, press)
        && phase <= 2
        && let Some(c) = crate::claude_slash::composer_of(r)
    {
        let text = p.slash.text();
        let (composer, echoed) = match c.text_and_typed() {
            ("", _) => (0, 0),
            (t, true) if text.starts_with(t) => {
                let want = if phase == 1 { "/" } else { text.as_str() };
                (1, i64::from(t == want))
            }
            _ => (2, 0),
        };
        observed.insert("composer", composer);
        observed.insert("echoed", echoed);
    }
    observed
}

fn drive_slash_action<T>(action: &str, drive: impl FnOnce() -> T) -> T {
    use aterm_spec::xref;

    assert!(xref::reset_entered_anchors());
    let result = drive();
    let entered = xref::entered_anchor_ids();
    xref::disarm_entered_anchors();
    let anchor = xref::refinements()
        .find(|anchor| anchor.machine == "claude_slash_press" && anchor.action == action)
        .expect("the slash press's action has a shipping anchor");
    assert!(
        entered.contains(anchor.entry_id),
        "{action} missed its shipping function"
    );
    result
}

fn check_slash_transition(
    app: &App,
    wid: WindowId,
    session: u64,
    action: &str,
    state: &mut aterm_spec::interp::State,
    historical: Option<&[(&'static str, i64)]>,
) {
    let model = aterm_models_gui::claude_slash_press_model();
    let before = state.clone();
    assert!(model.fire(action, state), "{action} is enabled");
    let observed = project_slash_press(app, wid, session, state);
    let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &observed,
        Some(action),
        "Claude slash press real conformance",
    );
    assert!(ok, "{action}: {evidence}");
    if let Some(changes) = historical {
        let mut old = observed.clone();
        for &(var, value) in changes {
            old.insert(var, value);
        }
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &old,
            Some(action),
            "Claude slash press negative control",
        );
        assert!(
            !ok,
            "{action} must reject the retired post-state {changes:?}"
        );
    }
}

#[test]
fn the_slash_press_conforms_to_its_model() {
    let model = aterm_models_gui::claude_slash_press_model();
    let slash = Slash::TopEffort(true);
    {
        let (mut app, wid, session, mut reader) = idle();
        let mut state = model.init_state();
        drive_slash_action("Click", || app.press_slash(wid, session, slash)).expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        check_slash_transition(
            &app,
            wid,
            session,
            "Click",
            &mut state,
            Some(&[("phase", 2), ("writes", 2)][..]),
        );
        echo(&app, session, CARET, "/");
        check_slash_transition(&app, wid, session, "Echo", &mut state, None);
        let at = Instant::now();
        drive_slash_action("Type", || app.advance_slash_presses(at));
        assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
        check_slash_transition(&app, wid, session, "Type", &mut state, None);
        echo(&app, session, CARET, &slash.text());
        check_slash_transition(&app, wid, session, "Echo", &mut state, None);
        app.advance_slash_presses(at);
        check_slash_transition(&app, wid, session, "Settle", &mut state, None);
        drive_slash_action("Enter", || app.advance_slash_presses(at + QUIET));
        assert_eq!(pty(&mut reader, 1), b"\r");
        check_slash_transition(
            &app,
            wid,
            session,
            "Enter",
            &mut state,
            Some(&[("unechoed", 1)][..]),
        );
        answered(&app, session, TOP, true);
        drive_slash_action("Done", || app.advance_slash_presses(at + QUIET));
        check_slash_transition(&app, wid, session, "Done", &mut state, None);
        drive_slash_action("Rest", || {
            app.advance_slash_presses(Instant::now() + NOTICE_SHOWN);
        });
        check_slash_transition(&app, wid, session, "Rest", &mut state, None);
        assert!(pty(&mut reader, 0).is_empty());
    }
    {
        const NORMAL: &str =
            include_str!("../../aterm-phase/src/fixtures/claude-2.1.304-top-vim-normal-120col.txt");
        let (mut app, wid, session, mut reader) = rig();
        draw_measured(&mut app, wid, session, NORMAL, |_| {}).expect("a caret");
        let mut state = model.init_state();
        check_slash_transition(&app, wid, session, "VimNormal", &mut state, None);
        drive_slash_action("Click", || app.press_slash(wid, session, slash)).expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        check_slash_transition(&app, wid, session, "Click", &mut state, None);
        assert!(model.successors("Type", &state).is_empty(), "no rest");
        let at = Instant::now() + ECHO;
        drive_slash_action("Stop", || app.advance_slash_presses(at));
        assert!(pty(&mut reader, 0).is_empty(), "no motion");
        check_slash_transition(
            &app,
            wid,
            session,
            "Stop",
            &mut state,
            Some(&[("named", 0)][..]),
        );
        let buggy = aterm_spec::interp::with_buggy(&model, 4);
        let mut raced = buggy.init_state();
        for action in ["VimNormal", "Click", "Type"] {
            assert!(buggy.fire(action, &mut raced), "{action}");
        }
        assert!(!buggy.check_invariant("NoMotionsInVim", &raced));
    }
    {
        let (mut app, wid, session, mut reader) = idle();
        let mut state = model.init_state();
        drive_slash_action("Click", || app.press_slash(wid, session, slash)).expect("begun");
        assert_eq!(pty(&mut reader, 1), b"/");
        check_slash_transition(&app, wid, session, "Click", &mut state, None);
        echo(&app, session, CARET, "/");
        check_slash_transition(&app, wid, session, "Echo", &mut state, None);
        mode_row_after_probe(&app, session, MANUAL_BUSY);
        check_slash_transition(&app, wid, session, "TurnStarts", &mut state, None);
        assert!(model.successors("Type", &state).is_empty(), "no rest");
        let at = Instant::now();
        app.advance_slash_presses(at);
        assert!(pty(&mut reader, 0).is_empty(), "nothing mid-turn");
        assert_eq!(
            project_slash_press(&app, wid, session, &state),
            state,
            "the App took no step"
        );
        mode_row_after_probe(&app, session, MANUAL);
        check_slash_transition(&app, wid, session, "TurnEnds", &mut state, None);
        drive_slash_action("Type", || app.advance_slash_presses(at));
        assert_eq!(pty(&mut reader, rest(slash).len()), rest(slash));
        check_slash_transition(&app, wid, session, "Type", &mut state, None);
    }
}

/// The top rule's key, clicked: its ghost and Claude's tag.
#[path = "claude_top_key_tests.rs"]
mod top_key;
