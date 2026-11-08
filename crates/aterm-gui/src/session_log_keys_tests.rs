// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a key chose on the box or question it landed on (fix E8): the key's class, the frame's
//! options as presence's adapter publishes them from `aterm-phase`'s reader, and the frame a key
//! lands on.

use std::sync::Arc;
use std::time::Instant;

use aterm_slog::Chose;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

use super::{FrameKeys, KeyClass, KeyWitness};
use crate::input::InputEvent;

/// `aterm-phase`'s committed screens, found at run time (`build-path-value`).
fn fixtures() -> std::path::PathBuf {
    aterm_tempfile::manifest_dir().join("../aterm-phase/src/fixtures")
}

/// The rows of the committed screen `name` (`<name>.txt` under [`fixtures`]), as the reader's own
/// fixture tests take them.
pub(crate) fn fixture_rows(name: &str) -> Vec<String> {
    let path = fixtures().join(format!("{name}.txt"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    aterm_phase::prompt::fixtures::screen(&text)
}

/// The keys presence's adapter publishes for the Claude Code screen `fixture`, as the sweep reads
/// it ([`crate::presence::agent_verdict`]).
pub(crate) fn frame(fixture: &str) -> Option<FrameKeys> {
    let rows = fixture_rows(fixture);
    crate::presence::agent_verdict(
        Some("claude"),
        false,
        None,
        &rows,
        crate::presence::Cursor::Unknown,
        "",
        Instant::now(),
    )
    .turn()
    .keys
}

fn key(key: Key, mods: Modifiers) -> InputEvent {
    InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

const ENTER: KeyClass = KeyClass::Enter;
const ESC: KeyClass = KeyClass::Esc;
const OTHER: KeyClass = KeyClass::Other;
/// A fixture and what each key chooses on it.
type Row<'a> = (&'a str, &'a [(KeyClass, Option<Chose>)]);

const fn digit(d: u8) -> KeyClass {
    KeyClass::Digit(d)
}

/// THE READING, ON REAL SCREENS: Enter chooses the focused option by its role, a digit the option
/// it numbers, Esc the cancel the footer names; a question's chosen option answers it, its review's
/// Cancel declines it; what no option places chooses nothing.
#[test]
fn a_key_chooses_what_the_screen_shows_it_choosing() {
    use Chose::{Answer, Approve, Decline};
    let rows: &[Row<'_>] = &[
        (
            // `❯ 1. Yes`, `2. No`, `Esc to cancel`.
            "claude-2.1.282-box-bash-ask-rule",
            &[
                (ENTER, Some(Approve)),
                (digit(1), Some(Approve)),
                (digit(2), Some(Decline)),
                (digit(3), None),
                (ESC, Some(Decline)),
                (OTHER, None),
            ],
        ),
        (
            // `❯ 1. Yes`, `2. Yes, and always allow`, and a third the capture garbled
            // (`3. Nooject`): an option of no role chooses nothing.
            "claude-2.1.280-box-bash-touch",
            &[
                (ENTER, Some(Approve)),
                (digit(2), Some(Approve)),
                (digit(3), None),
                (ESC, Some(Decline)),
            ],
        ),
        (
            // `❯ 1. Yes`, `2. Yes, and switch to accept edits … (shift+tab)`, `3. No`.
            "claude-2.1.280-box-edit",
            &[
                (ENTER, Some(Approve)),
                (digit(2), Some(Approve)),
                (digit(3), Some(Decline)),
            ],
        ),
        (
            // The vendor's default-to-No box: `❯ No` focused above `Yes`, unnumbered.
            "claude-2.1.282-box-tool-default-no",
            &[
                (ENTER, Some(Decline)),
                (digit(1), None),
                (ESC, Some(Decline)),
            ],
        ),
        (
            // An MCP server's dialog, `❯ Continue without using this MCP server` focused: the
            // reader reads its every option as kept, and none affirms.
            "claude-2.1.301-mcp-server-one",
            &[(ENTER, None), (ESC, Some(Decline))],
        ),
        (
            // A setting with a kept answer on both sides: `❯ 1. Yes, and keep allowing …` and
            // `4. Yes, but ask again next time` approve, `3. No, and ask again next time`
            // declines, and `2. No, and block reads outside the working directories …` keeps a
            // refusal, which the reader reads as kept, as it does the kept yes: it chooses
            // nothing (the review's finding: it approved).
            "claude-2.1.287-read-outside-setting-measured",
            &[
                (ENTER, Some(Approve)),
                (digit(1), Some(Approve)),
                (digit(2), None),
                (digit(3), Some(Decline)),
                (digit(4), Some(Approve)),
            ],
        ),
        (
            // 2.1.282's: `2. No, block reads outside the working directories …`.
            "claude-2.1.282-read-outside-setting",
            &[
                (ENTER, Some(Approve)),
                (digit(2), None),
                (digit(3), Some(Decline)),
            ],
        ),
        (
            // A box of one option (`❯ Continue without browser tools`): Enter is no choice.
            "claude-2.1.302-chrome-setup-connecting",
            &[(ENTER, None)],
        ),
        (
            // A single-choice question, `❯ 1. Green` focused; 3 is free text, 4 the chat.
            "claude-2.1.281-ask-one",
            &[
                (ENTER, Some(Answer)),
                (digit(1), Some(Answer)),
                (digit(2), Some(Answer)),
                (digit(3), None),
                (digit(4), None),
                (OTHER, None),
            ],
        ),
        (
            // The review: `❯ 1. Submit answers`, `2. Cancel`.
            "claude-2.1.282-question-review",
            &[
                (ENTER, Some(Answer)),
                (digit(1), Some(Answer)),
                (digit(2), Some(Decline)),
            ],
        ),
    ];
    let mut failures = Vec::new();
    for (fixture, keys) in rows {
        let frame = frame(fixture);
        assert!(frame.is_some(), "{fixture}: presence published no frame");
        for (class, want) in *keys {
            let got = frame.and_then(|f| f.chose(*class));
            if got != *want {
                failures.push(format!("{fixture}: {class:?} chose {got:?}, want {want:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A refusal's opening words as a person reads them: this test's own reading of the corpus's
/// labels, never the log's.
fn refuses(label: &str) -> bool {
    let l = label.trim().replace('\u{2019}', "'").to_lowercase();
    l == "no"
        || [
            "no,",
            "no ",
            "don't",
            "do not",
            "never",
            "deny",
            "reject",
            "not now",
            "cancel",
            "continue without",
            "exit",
            "quit",
        ]
        .iter()
        .any(|w| l.starts_with(w))
}

/// NO REFUSAL READS AS A YES ON ANY COMMITTED BOX (the lander's review of E8): over every agent's
/// committed screens, as each program's own reader parses the box, an option whose words refuse
/// never chooses `approve`, and one whose words affirm never chooses `decline`. The reader gives
/// the kept roles by the words that keep an answer, a refusal's as well as a grant's, so the
/// corpus must hold a refusal of a kept role, or this test checks nothing.
#[test]
fn no_option_whose_words_refuse_reads_as_approving_on_any_committed_box() {
    let dir = fixtures();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    let (mut boxes, mut kept_refusals) = (0usize, 0usize);
    let mut failures = Vec::new();
    for name in &names {
        let Some(stem) = name.strip_suffix(".txt") else {
            continue;
        };
        let Some(program) = ["claude", "agy", "muse", "grok", "kimi"]
            .into_iter()
            .find(|p| stem.starts_with(&format!("{p}-")))
        else {
            continue;
        };
        let rows = fixture_rows(stem);
        let Some(p) = aterm_phase::identify(Some(program), &rows).prompt(&rows) else {
            continue;
        };
        if p.kind == aterm_phase::PromptKind::Question {
            continue;
        }
        boxes += 1;
        for o in &p.options {
            let chose = super::option_chose(o);
            if refuses(&o.label) && o.role.standing() {
                kept_refusals += 1;
            }
            if (refuses(&o.label) && chose == Some(Chose::Approve))
                || (o.affirms() && chose == Some(Chose::Decline))
            {
                failures.push(format!(
                    "{stem}: {:?} {:?} chooses {chose:?}",
                    o.role, o.label
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(boxes > 100, "the corpus's boxes: {boxes}");
    assert!(
        kept_refusals > 0,
        "the corpus holds no refusal the reader reads as kept: this test checks nothing"
    );
}

/// A screen with no box or question publishes no frame: a key there chooses nothing.
#[test]
fn a_screen_with_no_box_publishes_no_frame() {
    assert_eq!(frame("claude-2.1.316-box-bash-declined-esc"), None);
}

/// THE KEY'S CLASS: a plain press of Enter, Esc or a digit, as a person's event, the bytes a
/// control verb writes, or a control verb's body; anything else (another key, a key under a
/// modifier, a release, typed words, a paste) is no choosing key.
#[test]
fn a_keys_class_is_its_plain_press() {
    let none = Modifiers::empty();
    let cases = [
        (key(Key::Named(NamedKey::Enter), none), ENTER),
        (key(Key::Named(NamedKey::Escape), none), ESC),
        (key(Key::Character('3'), none), digit(3)),
        (key(Key::Character('0'), none), OTHER),
        (key(Key::Character('y'), none), OTHER),
        (key(Key::Named(NamedKey::ArrowDown), none), OTHER),
        (key(Key::Named(NamedKey::Enter), Modifiers::SHIFT), OTHER),
        (
            key(Key::Named(NamedKey::Enter), Modifiers::CAPS_LOCK),
            ENTER,
        ),
        (InputEvent::Text("2".into()), digit(2)),
        (InputEvent::Text("12".into()), OTHER),
        (InputEvent::KeySequence(vec![0x0d]), ENTER),
        (InputEvent::KeySequence(vec![0x1b]), ESC),
        (InputEvent::KeySequence(vec![0x1b, b'[', b'B']), OTHER),
        (
            InputEvent::Paste("1".into(), crate::input::PasteFraming::AtDrain),
            OTHER,
        ),
    ];
    for (ev, want) in cases {
        assert_eq!(KeyClass::of_event(&ev), want, "{ev:?}");
    }
    let released = InputEvent::Key {
        key: Key::Named(NamedKey::Enter),
        mods: none,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Release,
    };
    assert_eq!(KeyClass::of_event(&released), OTHER, "a release");
    for (verb, body, want) in [
        ("key", "enter", ENTER),
        ("key", "esc", ESC),
        ("key", "2", digit(2)),
        ("key", "down", OTHER),
        ("key", "enter mods=ctrl", OTHER),
        ("send", "\\n", ENTER),
        ("send", "1", digit(1)),
        ("send", "yes\\n", OTHER),
        ("feed", "0d", ENTER),
        ("feed", "1b", ESC),
        ("feed", "zz", OTHER),
        ("paste", "1", OTHER),
        ("ctrl", "c", OTHER),
    ] {
        assert_eq!(
            crate::control::key_class(verb, body),
            want,
            "{verb} {body:?}"
        );
    }
}

/// THE FRAME A KEY LANDS ON: the last read stands for the next key; an input voids it; a read
/// the screen did not move after the input (taken between its write and the program's redraw)
/// does not restore it; a read after the screen moved does. A control input admitted on a frame
/// chooses nothing when another input reached the program before its acceptance. A screen that
/// moved after the last read, with no input since, voids it too, until a read sees the move (a
/// read of the same zone keeps the frame at the moved count).
#[test]
fn a_frame_stands_only_until_an_input_and_counts_again_once_the_screen_moved() {
    let moves = Arc::new(crate::vitals_host::ScreenMoves::default());
    let feed = |n: u64| {
        assert!(moves.fed(crate::control::ScreenGen { epoch: 0, seq: n }));
        moves.count()
    };
    let yes = frame("claude-2.1.280-box-bash-touch");
    let no = frame("claude-2.1.282-box-tool-default-no");
    let mut w = KeyWitness::default();
    w.bind(Arc::clone(&moves));
    assert_eq!(w.chose(ENTER), None, "no read yet");
    let m = feed(1);
    w.read(yes, m);
    assert_eq!(
        w.chose(ENTER),
        Some(Chose::Approve),
        "the read before the key"
    );
    let admitted = w.at_admission();
    w.reached();
    assert_eq!(w.chose(ENTER), None, "the frame the input landed on");
    w.read(yes, m);
    assert_eq!(w.chose(ENTER), None, "a read the input did not move");
    let m = feed(2);
    w.read(no, m);
    assert_eq!(w.chose(ENTER), Some(Chose::Decline), "the program's redraw");
    assert_eq!(
        admitted.chose(ENTER, &w),
        None,
        "admitted before another input reached the program"
    );
    let admitted = w.at_admission();
    assert_eq!(admitted.chose(ENTER, &w), Some(Chose::Decline));
    assert_eq!(admitted.chose(OTHER, &w), None, "typed text");
    w.read(None, feed(3));
    assert_eq!(w.chose(ENTER), None, "no box on screen");
    // The lander's review: the screen moved after the last read, and no input since.
    w.read(yes, feed(4));
    assert_eq!(w.chose(ENTER), Some(Chose::Approve), "the read of the box");
    let moved = feed(5);
    assert_eq!(
        w.chose(ENTER),
        None,
        "the box moved after the read, before the next one"
    );
    assert_eq!(
        w.at_admission().chose(ENTER, &w),
        None,
        "a control input admitted on it"
    );
    w.reread(moved);
    assert_eq!(
        w.chose(ENTER),
        Some(Chose::Approve),
        "a read of the same zone at the moved count"
    );
}
