// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;

use super::{
    ComposerAfterPress, ComposerKey, EchoWait, composer_after_press, composer_key_once_shown,
    composer_verify_key,
};

fn screen(caret_row: &str, col: u16) -> Terminal {
    let rule = "\u{2500}".repeat(80);
    let mut t = Terminal::new(24, 80);
    t.process(
        format!(
            "\x1b[20;1H{rule}\x1b[21;1H{caret_row}\x1b[22;1H{rule}\
             \x1b[23;1H  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle)\
             \x1b[21;{col}H"
        )
        .as_bytes(),
    );
    t
}

#[test]
fn the_key_is_the_typed_text_the_composer_shows() {
    let typed = screen("\u{276f} edit the README", 18);
    assert_eq!(
        composer_verify_key(&typed, "edit the  README"),
        ComposerKey::Shows("edittheREADME".to_string())
    );
    let empty = screen("\u{276f} ", 3);
    assert_eq!(composer_verify_key(&empty, "edit"), ComposerKey::NotYet);
    let chip = screen("\u{276f} [Pasted text #1 +12 lines] ", 30);
    assert_eq!(
        composer_verify_key(&chip, "a\nlong\npaste"),
        ComposerKey::No
    );
    assert_eq!(composer_verify_key(&typed, " \n"), ComposerKey::No);
    let mut boxed = Terminal::new(24, 80);
    boxed.process(" Select model\r\n \u{276f} 1. Default\r\n   2. Opus".as_bytes());
    assert_eq!(composer_verify_key(&boxed, "edit"), ComposerKey::No);
    let placeholder = screen("\u{276f} Press up to edit queued messages", 3);
    assert_eq!(
        composer_verify_key(&placeholder, "edit"),
        ComposerKey::NotYet
    );
    let drafted = screen("\u{276f} Press up to edit queued messages", 35);
    assert_eq!(
        composer_verify_key(&drafted, "edit"),
        ComposerKey::Shows("edit".to_string())
    );
}

#[test]
fn after_the_enter_the_composer_has_left_holds_or_is_gone() {
    assert_eq!(
        composer_after_press(&screen("\u{276f} ", 3), "edit"),
        ComposerAfterPress::Left
    );
    assert_eq!(
        composer_after_press(
            &screen("\u{276f} Press up to edit queued messages", 3),
            "edit"
        ),
        ComposerAfterPress::Left
    );
    assert_eq!(
        composer_after_press(&screen("\u{276f} edit", 7), "edit"),
        ComposerAfterPress::Holds
    );
    let mut boxed = Terminal::new(24, 80);
    boxed.process(" Select model\r\n \u{276f} 1. Default".as_bytes());
    assert_eq!(
        composer_after_press(&boxed, "edit"),
        ComposerAfterPress::Gone
    );
    let mut torn = screen("\u{276f} edit", 7);
    torn.process(b"\x1b[?2026h\x1b[21;1H\x1b[2K");
    assert_eq!(
        composer_after_press(&torn, "edit"),
        ComposerAfterPress::Unfinished
    );
}

#[test]
fn a_late_echo_is_waited_for_within_the_cap() {
    let script = |looks: Vec<ComposerKey>, moves: Vec<Result<bool, String>>| {
        let mut looks = looks.into_iter().enumerate();
        let mut moves = moves.into_iter();
        let mut waited = Vec::new();
        let got = composer_key_once_shown(
            || {
                let (seq, seen) = looks.next().expect("a look");
                (seen, 10 + seq as u64)
            },
            |after| {
                waited.push(after);
                moves.next().expect("a wait")
            },
        );
        (got, waited)
    };
    let shows = || ComposerKey::Shows("edit".to_string());
    assert_eq!(
        script(
            vec![ComposerKey::NotYet, ComposerKey::NotYet, shows()],
            vec![Ok(true), Ok(true)]
        ),
        (Ok(Some("edit".to_string())), vec![10, 11])
    );
    assert_eq!(
        script(
            vec![ComposerKey::NotYet, ComposerKey::NotYet],
            vec![Ok(true), Ok(false)]
        ),
        (Ok(None), vec![10, 11]),
        "the cap ends first"
    );
    assert_eq!(
        script(vec![ComposerKey::NotYet, ComposerKey::No], vec![Ok(true)]),
        (Ok(None), vec![10])
    );
    assert_eq!(
        script(
            vec![ComposerKey::NotYet],
            vec![Err("ERR exited\n".to_string())]
        ),
        (Err("ERR exited\n".to_string()), vec![10])
    );
    assert_eq!(
        script(vec![shows()], vec![]),
        (Ok(Some("edit".to_string())), vec![])
    );
}

#[test]
fn the_echo_cap_runs_from_the_first_look_that_finds_the_text_read() {
    let t0 = std::time::Instant::now();
    let s = std::time::Duration::from_secs;
    let (cap, far) = (EchoWait::CAP, t0 + s(600));

    let mut echo = EchoWait::after_typing(t0, far);
    assert_eq!(
        echo.look(false, t0 + cap),
        Some(t0 + cap + cap),
        "read before any look: the cap from the first look after it"
    );
    assert_eq!(
        echo.look(false, t0 + cap + s(1)),
        Some(t0 + cap + cap),
        "and no later one"
    );
    assert_eq!(echo.look(false, t0 + cap + cap), None, "its end passed");

    let mut echo = EchoWait::after_typing(t0, far);
    assert_eq!(
        echo.look(true, t0 + s(1)),
        Some(t0 + s(1) + cap),
        "unread: the cap from the look"
    );
    assert_eq!(echo.look(true, t0 + s(3)), Some(t0 + s(3) + cap));
    assert_eq!(
        echo.look(false, t0 + s(4)),
        Some(t0 + s(4) + cap),
        "the first look that finds it read"
    );
    assert_eq!(echo.look(false, t0 + s(5)), Some(t0 + s(4) + cap));
    assert_eq!(
        echo.look(true, t0 + s(6)),
        Some(t0 + s(6) + cap),
        "unread again"
    );

    let mut echo = EchoWait::after_typing(t0, far);
    let late = t0 + EchoWait::HANG - cap / 2;
    assert_eq!(
        echo.look(true, late),
        Some(t0 + EchoWait::HANG),
        "never past a minute after the typing"
    );
    assert_eq!(echo.look(true, t0 + EchoWait::HANG), None);

    let mut echo = EchoWait::after_typing(t0, t0 + s(2));
    assert_eq!(
        echo.look(true, t0 + s(1)),
        Some(t0 + s(2)),
        "nor past the deadline"
    );
}
