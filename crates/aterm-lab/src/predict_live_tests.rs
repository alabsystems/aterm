// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn screen(rows: &[&str], cursor: (usize, usize)) -> Screen {
    Screen {
        rows: rows.iter().map(|r| (*r).to_string()).collect(),
        cursor,
    }
}

fn press(t: u64, key: &str, armed: Option<u64>) -> Press {
    Press {
        t,
        key: key.to_string(),
        armed,
    }
}

fn frame(t: u64, newest: u64, under: &str, over: &str, cursor: usize, shown: usize) -> Frame {
    Frame {
        t,
        mode: "adaptive".to_string(),
        newest: Some(newest),
        cursor: (0, cursor),
        shown: (0, shown),
        rows: vec![(0, under.to_string(), over.to_string())],
    }
}

/// The trace's lines parse back to what aterm-gui's `predict_trace` wrote.
#[test]
fn the_trace_parses_back_into_presses_and_frames() {
    let log = "noise\n\
        aterm-predict press t=10 key=glyph%20x armed=3 pending=1\n\
        aterm-predict press t=11 key=submit armed=- pending=0\n\
        aterm-predict frame t=12 mode=adaptive engaged=1 visible=2 newest=3 cursor=0,2 shown=0,3 row=0 under=$ over=$%20x\n\
        aterm-predict clear t=13\n";
    let ev = parse_trace(log);
    assert_eq!(
        ev,
        vec![
            Event::Press(press(10, "glyph x", Some(3))),
            Event::Press(press(11, "submit", None)),
            Event::Frame(Frame {
                t: 12,
                mode: "adaptive".to_string(),
                newest: Some(3),
                cursor: (0, 2),
                shown: (0, 3),
                rows: vec![(0, "$".to_string(), "$ x".to_string())],
            }),
            Event::Clear(13),
        ]
    );
    assert_eq!(decode("a%25b%20%C3%A9"), "a%b é");
}

/// A take replays through aterm-core at the size it was told: the screen
/// before each input it read, and after the last output.
#[test]
fn a_take_replays_to_the_screen_before_each_input() {
    let take = parse_take("1 W 3x10\n2 O 2420\n3 I 61\n4 O 61\n5 I 0d\n6 O 0d0a6f6b\n");
    assert_eq!(take.len(), 6);
    let screens = replay(&take);
    assert_eq!(screens.len(), 3);
    assert_eq!(screens[0], screen(&["$", "", ""], (0, 2)));
    assert_eq!(screens[1], screen(&["$ a", "", ""], (0, 3)));
    assert_eq!(screens[2].rows[1], "ok");
}

/// A frame drawing the screen its newest key settles to is held and, its
/// engine still short of that screen, counts as AHEAD of the echo for every
/// key of its line it shows; one drawing another cell, or another cursor,
/// is WRONG (the negative controls).
#[test]
fn a_frame_is_held_to_its_newest_keys_settled_screen() {
    let presses = [
        press(100, "glyph a", Some(0)),
        press(200, "glyph b", Some(1)),
        press(300, "submit", None),
    ];
    let settled = [
        screen(&["$ a"], (0, 3)),
        screen(&["$ ab"], (0, 4)),
        screen(&["$ ab"], (1, 0)),
    ];
    let good = frame(250, 1, "$", "$ ab", 2, 4);
    let v = judge(&presses, std::slice::from_ref(&good), &settled);
    assert!(v.wrong.is_empty(), "{:?}", v.wrong);
    assert_eq!((v.checked, v.ahead), (1, 1));
    assert_eq!(v.keys.get("glyph"), Some(&(2, 2)), "both keys shown early");
    assert_eq!(v.perceived_us, vec![150, 50]);

    let caught = frame(250, 1, "$", "$ ax", 2, 4);
    let v = judge(&presses, &[caught], &settled);
    assert_eq!(v.wrong.len(), 1, "a wrong cell is caught");
    assert!(v.wrong[0].contains("settles to \"$ ab\""), "{}", v.wrong[0]);
    let misplaced = frame(250, 1, "$", "$ ab", 2, 3);
    assert_eq!(
        judge(&presses, &[misplaced], &settled).wrong.len(),
        1,
        "a wrong cursor is caught"
    );

    let late = frame(260, 0, "$ a", "$ a", 3, 3);
    let v = judge(&presses, &[late], &settled);
    assert!(v.wrong.is_empty());
    assert_eq!((v.checked, v.ahead), (1, 0));
    assert_eq!(v.keys.get("glyph"), Some(&(2, 0)));

    let stray = Frame {
        newest: Some(9),
        ..good
    };
    assert_eq!(judge(&presses, &[stray], &settled).unattributed, 1);
}

/// A painted frame its `clear` erased is gone by the next line; one never
/// erased is still on glass there and at the end (the negative control).
#[test]
fn a_prediction_must_be_erased_before_the_next_line() {
    let f = frame(10, 0, "$", "$ a", 2, 3);
    let erased = [
        Event::Press(press(1, "glyph a", Some(0))),
        Event::Frame(f.clone()),
        Event::Press(press(20, "submit", None)),
        Event::Clear(30),
        Event::Press(press(40, "glyph b", Some(1))),
    ];
    assert!(left_on_glass(&erased).is_empty());
    let mid_line = [
        Event::Press(press(1, "glyph a", Some(0))),
        Event::Frame(f.clone()),
        Event::Press(press(5, "glyph b", Some(1))),
        Event::Clear(30),
    ];
    assert!(
        left_on_glass(&mid_line).is_empty(),
        "a frame on glass as the line goes on is the line's own"
    );
    let stale = [
        Event::Press(press(1, "glyph a", Some(0))),
        Event::Frame(f),
        Event::Press(press(20, "submit", None)),
        Event::Press(press(40, "glyph b", Some(1))),
    ];
    let left = left_on_glass(&stale);
    assert_eq!(
        left.len(),
        2,
        "at the next line's first key, and at the end: {left:?}"
    );
    assert!(left[0].contains("t=40us"), "{}", left[0]);
}

/// The corpus is the plan's kinds, every line ending in Enter, and its
/// cadence puts a notice before an edit key and before Enter.
#[test]
fn the_corpus_holds_the_plans_kinds_and_a_typists_cadence() {
    let lines = corpus(2);
    assert_eq!(lines.len(), 20);
    assert!(
        lines
            .iter()
            .all(|l| l.keys.last() == Some(&Key::Named("enter")))
    );
    let families: Vec<&str> = corpus(1).iter().map(|l| l.family).collect();
    assert_eq!(
        families,
        [
            "plain",
            "typo-end",
            "left-right",
            "mid-insert",
            "mid-erase",
            "ctrl-d",
            "delete",
            "home-end",
            "ctrl-a-e",
            "typo-back"
        ]
    );
    assert_eq!(Key::Ctrl('a').hwkey(), "hwkey a mods=ctrl");
    assert_eq!(Key::Ch(' ').key(), "key space");
    assert_eq!(gap(None, Key::Ch('e'), 0), Duration::ZERO);
    assert_eq!(
        gap(Some(Key::Ch('e')), Key::Named("left"), 3),
        Duration::from_millis(350)
    );
    assert_eq!(
        gap(Some(Key::Ch('e')), Key::Ch('c'), 1),
        Duration::from_millis(130)
    );
    assert_eq!(quantile(&[5, 1, 9, 3], 0.5), 3);
    assert_eq!(quantile(&[], 0.5), 0);
}

fn phase(verdict: Option<Verdict>, predicted: Option<u64>, off_held: bool) -> Phase {
    Phase {
        name: "single pane",
        verdict,
        metrics: String::new(),
        predicted,
        oracle: String::new(),
        notes: Vec::new(),
        off_held,
    }
}

fn on(rtt: u64) -> Config {
    Config {
        shell: "zsh".to_string(),
        rtt,
        on: true,
    }
}

/// A judged phase that showed what it is run to show: frames painted and
/// held, keys shown before their echo, and their input slices booked at
/// their prediction.
fn shown() -> Verdict {
    let presses = [press(100, "glyph a", Some(0)), press(300, "submit", None)];
    let settled = [screen(&["$ a"], (0, 3)), screen(&["$ a"], (1, 0))];
    judge(&presses, &[frame(150, 0, "$", "$ a", 2, 3)], &settled)
}

/// The `metrics` reply's fields are read whole, never a prefix of another.
#[test]
fn a_metric_is_read_by_its_whole_name() {
    let reply = "OK n_input=537 input_predicted=401 input_p50_ms=1.44";
    assert_eq!(metric(reply, "input_predicted"), Some("401"));
    assert_eq!(metric(reply, "input_p50_ms"), Some("1.44"));
    assert_eq!(metric(reply, "input"), None);
    assert_eq!(
        metric_line(reply).split(' ').nth(1),
        Some("input_predicted=401")
    );
}

/// NOT VACUOUSLY.
#[test]
fn a_phase_holds_only_when_it_showed_what_it_is_run_to_show() {
    let held = phase(Some(shown()), Some(1), false);
    assert_eq!(failures(&on(100), true, &held), Vec::<String>::new());

    let quiet = || phase(Some(Verdict::default()), Some(0), false);
    let failed = failures(&on(100), true, &quiet());
    assert_eq!(failed.len(), 3, "{failed:?}");
    assert!(failed[0].contains("no frame painted"), "{failed:?}");
    assert!(
        failures(&on(100), false, &quiet()).is_empty(),
        "headless never presents"
    );
    assert!(
        failures(&on(MUST_PAINT_RTT_MS - 1), true, &quiet()).is_empty(),
        "below the gate's round trip, painting is its call"
    );

    let unwired = phase(Some(shown()), Some(0), false);
    assert_eq!(
        failures(&on(100), true, &unwired),
        vec!["input_predicted=0: no key's input slice closed at its prediction".to_string()]
    );

    let mut stray = shown();
    stray.unattributed = 1;
    let failed = failures(&on(100), true, &phase(Some(stray), Some(1), false));
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].contains("name no press"), "{failed:?}");

    let off = Config {
        on: false,
        ..on(100)
    };
    assert!(failures(&off, true, &phase(None, Some(0), true)).is_empty());
    let failed = failures(&off, true, &phase(None, Some(3), true));
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].contains("input_predicted=3"), "{failed:?}");
    assert_eq!(
        failures(&off, true, &phase(None, Some(0), false)).len(),
        1,
        "a frame painted with prediction off"
    );
}
