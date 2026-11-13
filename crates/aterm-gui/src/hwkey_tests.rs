// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aterm_window::event::InputOrigin;

use super::{
    CURRENT_INPUT_STUB, FLAG_CONTROL, FLAG_FUNCTION, FLAG_SHIFT, HID_SYSTEM, LEDGER_CAP, Ledger,
    MAX_COUNT, NS_EVENT_TYPE_KEY_DOWN, NS_EVENT_TYPE_KEY_UP, NS_EVENT_TYPE_LEFT_MOUSE_UP,
    NativeInput, classify, current_event_origin, note_posted, parse_hwkey, run_burst,
};

/// A FULL LEDGER REFUSES KEYS RATHER THAN FORGET ONE: every posted key must be found when
/// AppKit dispatches it, however many wait ahead of it, or it runs as the person's (its ⌘V
/// pastes the clipboard). So once the ledger waits for all it can hold, a burst's next press
/// is refused whole, unposted (`ERR hwkey: busy`), and the oldest waiting key is still found;
/// a key found clears every key of its kind posted before it (AppKit took those unseen, an
/// input method say), which makes room again, and leaves the other kind waiting.
/// NEGATIVE CONTROL: with `Ledger::note` dropping the oldest waiting key at the cap and
/// taking the new one (as the branch had it at `0e59d97e00`), the press past the cap was
/// taken.
#[test]
fn a_full_ledger_refuses_keys_rather_than_forget_one() {
    let key = |n: u32, kind: usize| NativeInput {
        kind,
        keycode: 9,
        timestamp: 3_000.0 + f64::from(n),
        source_pid: HID_SYSTEM,
    };
    let half = u32::try_from(LEDGER_CAP / 2).expect("a small cap");
    let down = |n| key(n, NS_EVENT_TYPE_KEY_DOWN);
    let up = |n| key(n, NS_EVENT_TYPE_KEY_UP);

    let mut ledger = Ledger::default();
    for n in 0..half {
        assert!(ledger.note(&[down(n), up(n)]), "press {n}");
    }
    assert_eq!(ledger.room(), 0, "PRECONDITION: full");
    assert!(!ledger.note(&[down(half), up(half)]), "past the cap");
    assert!(ledger.take(&down(0)), "the oldest key is still found");
    assert!(!ledger.note(&[down(half), up(half)]), "no room for a press");

    assert!(ledger.take(&down(9)), "a later key-down");
    assert_eq!(
        ledger.room(),
        1 + 9,
        "it and the key-downs before it are gone"
    );
    assert!(!ledger.take(&down(5)), "a key-down AppKit took unseen");
    assert!(ledger.take(&up(5)), "the key-ups wait on");
    assert!(ledger.note(&[down(half), up(half)]), "room again");

    let mut refused = Ok(());
    for n in 0..=half {
        refused = note_posted(&[down(n), up(n)]);
        if refused.is_err() {
            assert_eq!(n, half, "refused only at the cap");
        }
    }
    let refused = refused.expect_err("a burst past the cap is refused");
    assert!(refused.starts_with("ERR hwkey: busy"), "{refused}");
}

/// THE MAIN THREAD TELLS A POSTED KEY FROM A PERSON'S: a key event the burst noted is found
/// as it is dispatched, with a stamp CoreGraphics' integer clock moved by a nanosecond, and
/// again on a second look at the same event (a menu's key equivalent, then a view); the
/// person's key with another stamp, kind or keycode is not, and takes nothing from the ledger;
/// a found key is not found again once another was.
/// NEGATIVE CONTROL: with `STAMP_SLACK` at 0 the posted key with its rounded stamp was not
/// found.
#[test]
fn the_main_thread_tells_a_posted_key_from_a_persons() {
    let at = |key: NativeInput| {
        CURRENT_INPUT_STUB.with(|current| current.set(Some(key)));
        let posted = current_event_origin() == InputOrigin::Posted;
        CURRENT_INPUT_STUB.with(|current| current.set(None));
        posted
    };
    let down = NativeInput {
        kind: NS_EVENT_TYPE_KEY_DOWN,
        keycode: 9,
        timestamp: 5_000.123_456_789,
        source_pid: HID_SYSTEM,
    };
    note_posted(&[down]).expect("room");
    assert_eq!(
        current_event_origin(),
        InputOrigin::Person,
        "no event is being dispatched"
    );
    let later = NativeInput {
        timestamp: 5_000.2,
        ..down
    };
    assert!(!at(later), "the person's ⌘V a moment later");
    let up = NativeInput {
        kind: NS_EVENT_TYPE_KEY_UP,
        ..down
    };
    assert!(!at(up), "another kind");
    assert!(!at(NativeInput { keycode: 8, ..down }), "another key");
    let rounded = NativeInput {
        timestamp: 5_000.123_456_79,
        ..down
    };
    assert!(
        at(rounded),
        "the posted key, its stamp in whole nanoseconds"
    );
    assert!(at(down), "a second look at it");

    note_posted(&[up]).expect("room");
    assert!(at(up), "its key-up");
    assert!(!at(down), "the key-down, once the key-up was found");
}

/// INPUT ANOTHER PROCESS POSTED IS RELAYED, A BURST'S KEY POSTED: a key or click whose source
/// is neither the HID system nor this process (System Events' `keystroke` or `click`, a tab's
/// program's `CGEventPost`, Screen Sharing) is relayed though no burst noted it, both to a
/// native monitor and as the event in dispatch (a menu's key equivalent, or the click that
/// chose the item), and it takes nothing from the ledger; the same key or click from the HID
/// system, or a key from this process no burst noted, is the person's; the burst's key is
/// still found, and is the burst's.
/// NEGATIVE CONTROL: with `NativeInput::relayed` answering false (the classifier as it was
/// before it read the source), another process's key and click were the person's.
#[test]
fn input_another_process_posted_is_relayed_and_a_bursts_key_posted() {
    let here = i64::from(std::process::id());
    let key = |n: u32, source_pid: i64| NativeInput {
        kind: NS_EVENT_TYPE_KEY_DOWN,
        keycode: 1,
        timestamp: 7_000.0 + f64::from(n),
        source_pid,
    };
    let click = |n: u32, source_pid: i64| NativeInput {
        kind: NS_EVENT_TYPE_LEFT_MOUSE_UP,
        keycode: 0,
        ..key(n, source_pid)
    };
    let at = |event: NativeInput| {
        CURRENT_INPUT_STUB.with(|current| current.set(Some(event)));
        let origin = current_event_origin();
        CURRENT_INPUT_STUB.with(|current| current.set(None));
        origin
    };
    let burst = key(0, here);
    note_posted(&[burst]).expect("room");

    assert_eq!(
        classify(key(1, here + 1)),
        InputOrigin::Relayed,
        "held by a native monitor"
    );
    assert_eq!(at(key(2, here + 1)), InputOrigin::Relayed, "in dispatch");
    assert_eq!(at(click(3, here + 1)), InputOrigin::Relayed, "a click");
    assert_eq!(at(key(4, HID_SYSTEM)), InputOrigin::Person, "a keyboard's");
    assert_eq!(at(click(5, HID_SYSTEM)), InputOrigin::Person, "a mouse's");
    assert_eq!(
        classify(key(6, here)),
        InputOrigin::Person,
        "this process's key no burst noted"
    );
    assert_eq!(at(burst), InputOrigin::Posted, "the burst's key is found");
}

/// ANOTHER PROCESS'S INPUT IS RELAYED BEFORE ANY BURST: in a process (here, a test thread)
/// where no burst has noted a key, the ledger is never consulted, and a key or click from
/// another process is still relayed.
/// NEGATIVE CONTROL: with the relayed check behind the noted gate (`noted() && (burst ||
/// relayed)`, the gate's shape before the source was read), both were the person's.
#[test]
fn another_processs_input_is_relayed_before_any_burst_noted_a_key() {
    let elsewhere = i64::from(std::process::id()) + 1;
    let key = NativeInput {
        kind: NS_EVENT_TYPE_KEY_DOWN,
        keycode: 1,
        timestamp: 7_500.0,
        source_pid: elsewhere,
    };
    assert!(!super::noted(), "PRECONDITION: no burst noted a key here");
    assert_eq!(classify(key), InputOrigin::Relayed);
    let click = NativeInput {
        kind: NS_EVENT_TYPE_LEFT_MOUSE_UP,
        keycode: 0,
        ..key
    };
    assert_eq!(classify(click), InputOrigin::Relayed);
    assert_eq!(
        classify(NativeInput {
            source_pid: HID_SYSTEM,
            ..key
        }),
        InputOrigin::Person
    );
}

/// A native menu or toolbar callback in a nested loop reads its own current key, while a
/// deferred window callback keeps its original source. Neither reader borrows the other's
/// decision, including the physical callback's definitive person's verdict.
#[test]
fn native_readers_do_not_borrow_a_retained_window_callbacks_source() {
    let a = NativeInput {
        kind: NS_EVENT_TYPE_KEY_DOWN,
        keycode: 0,
        timestamp: 6_700.0,
        source_pid: HID_SYSTEM,
    };
    let physical_b = NativeInput {
        timestamp: 6_701.0,
        ..a
    };
    let posted_b = NativeInput {
        timestamp: 6_702.0,
        ..a
    };
    note_posted(&[a, posted_b]).expect("room");
    let retained_a = classify(a);
    assert_eq!(retained_a, InputOrigin::Posted);
    CURRENT_INPUT_STUB.with(|current| current.set(Some(physical_b)));
    super::with_window_event_origin_for_test(retained_a, || {
        assert_eq!(
            current_event_origin(),
            InputOrigin::Person,
            "the nested native key is physical"
        );
        assert_eq!(
            super::window_callback_origin(),
            InputOrigin::Posted,
            "the window callback still belongs to A"
        );
        CURRENT_INPUT_STUB.with(|current| current.set(Some(posted_b)));
        super::with_window_event_origin_for_test(InputOrigin::Person, || {
            assert_eq!(
                current_event_origin(),
                InputOrigin::Posted,
                "the nested native key is posted"
            );
            assert_eq!(
                super::window_callback_origin(),
                InputOrigin::Person,
                "the physical callback stays physical"
            );
        });
        CURRENT_INPUT_STUB.with(|current| current.set(Some(physical_b)));
        assert_eq!(
            classify(a),
            InputOrigin::Person,
            "PRECONDITION: B consumed A's raw ledger identity"
        );
        assert_eq!(
            super::window_callback_origin(),
            InputOrigin::Posted,
            "the enclosing callback is restored"
        );
        assert_eq!(current_event_origin(), InputOrigin::Person);
    });
    assert_eq!(
        super::window_callback_origin(),
        InputOrigin::Person,
        "outside a callback the raw reader is used"
    );
    CURRENT_INPUT_STUB.with(|current| current.set(None));
}

/// System State (or a gateway card) coming to the front after the second key of five stops
/// the burst, which says how many it posted; every posted key was counted before its post.
/// Control: with nothing of the person's in front all five post, and a front already up posts
/// none.
#[test]
fn a_running_burst_stops_once_the_persons_buttons_come_to_the_front() {
    let front = AtomicBool::new(false);
    let posted = AtomicU64::new(0);
    let mut sent = 0;
    let out = run_burst(
        5,
        0,
        &front,
        || false,
        &posted,
        |_| {
            sent += 1;
            if sent == 2 {
                // The page came to the front: the main thread's word.
                front.store(true, Ordering::Release);
            }
            Ok(())
        },
    );
    let refusal = out.expect_err("stopped at the page");
    assert!(refusal.contains("posted=2"), "{refusal}");
    assert_eq!(sent, 2, "nothing posted after it came to the front");
    assert_eq!(posted.load(Ordering::Acquire), 2);

    let quiet = AtomicBool::new(false);
    let mut sent = 0;
    let all = run_burst(
        5,
        0,
        &quiet,
        || false,
        &posted,
        |_| {
            sent += 1;
            Ok(())
        },
    );
    assert_eq!(all, Ok(5));
    assert_eq!(posted.load(Ordering::Acquire), 7);

    let mut sent = 0;
    let none = run_burst(
        5,
        0,
        &front,
        || false,
        &posted,
        |_| {
            sent += 1;
            Ok(())
        },
    );
    assert!(none.is_err_and(|e| e.contains("posted=0")));
    assert_eq!(sent, 0);
}

/// One of the person's confirmations coming up after the second key of five (a posted ⌘W
/// whose Close question opened) stops the burst before the third, and the refusal says how
/// many it posted; one already up posts none. NEGATIVE CONTROL: with `run_burst`'s
/// `confirmation_up` gate removed, all five posted and the burst answered `Ok(5)`.
#[test]
fn a_running_burst_stops_once_a_confirmation_comes_up() {
    let front = AtomicBool::new(false);
    let up = AtomicBool::new(false);
    let posted = AtomicU64::new(0);
    let mut told = Vec::new();
    let out = run_burst(
        5,
        0,
        &front,
        || up.load(Ordering::Acquire),
        &posted,
        |sent| {
            told.push(sent);
            if sent == 1 {
                up.store(true, Ordering::Release);
            }
            Ok(())
        },
    );
    let refusal = out.expect_err("stopped at the question");
    assert!(refusal.contains("posted=2"), "{refusal}");
    assert!(refusal.contains("confirmation"), "{refusal}");
    assert_eq!(told, [0, 1], "each post is told how many came before it");
    assert_eq!(posted.load(Ordering::Acquire), 2);

    let mut sent = 0;
    let none = run_burst(
        5,
        0,
        &front,
        || up.load(Ordering::Acquire),
        &posted,
        |_| {
            sent += 1;
            Ok(())
        },
    );
    assert!(none.is_err_and(|e| e.contains("posted=0")));
    assert_eq!(sent, 0);
}

#[test]
fn a_plain_letter_is_its_unshifted_physical_key() {
    let s = parse_hwkey("x").expect("x parses");
    assert_eq!(s.keycode, 7, "kVK_ANSI_X");
    assert_eq!(s.chars, "x");
    assert_eq!(s.chars_ignoring, "x");
    assert_eq!(s.flags, 0);
    assert_eq!(s.count, 1);
}

#[test]
fn an_uppercase_letter_shifts_the_same_physical_key() {
    for (plain, shifted) in [("x", "X"), ("1", "!")] {
        let lower = parse_hwkey(plain).unwrap();
        let upper = parse_hwkey(shifted).unwrap();
        assert_eq!(upper.keycode, lower.keycode, "{shifted}");
        assert_eq!(upper.flags, FLAG_SHIFT, "{shifted}");
        assert_eq!(upper.chars_ignoring, shifted);
    }
}

#[test]
fn a_control_chord_carries_the_c0_byte_and_the_bare_letter() {
    let a = parse_hwkey("ctrl+u").unwrap();
    let b = parse_hwkey("u mods=ctrl").unwrap();
    assert_eq!(a, b);
    assert_eq!(a.flags, FLAG_CONTROL);
    assert_eq!(a.chars, "\u{15}");
    assert_eq!(a.chars_ignoring, "u");
}

#[test]
fn named_keys_match_appkit_codes_and_function_flag() {
    let enter = parse_hwkey("enter").unwrap();
    assert_eq!(enter.keycode, 36);
    assert_eq!(enter.chars, "\r");
    assert_eq!(
        enter.flags & FLAG_FUNCTION,
        0,
        "Return is not a function key"
    );

    let up = parse_hwkey("up").unwrap();
    assert_eq!(up.keycode, 126);
    assert_eq!(up.chars, "\u{f700}");
    assert_eq!(up.flags & FLAG_FUNCTION, FLAG_FUNCTION);

    assert_eq!(parse_hwkey("esc").unwrap(), parse_hwkey("escape").unwrap());
    assert_eq!(
        parse_hwkey("enter").unwrap(),
        parse_hwkey("return").unwrap()
    );
}

#[test]
fn no_accepted_key_yields_empty_character_strings() {
    for spelling in [
        "a",
        "Z",
        "0",
        "!",
        "space",
        "enter",
        "tab",
        "esc",
        "backspace",
        "delete",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "pageup",
        "pagedown",
        "f1",
        "f12",
        "ctrl+c",
        "cmd+v",
        "shift+alt+q",
    ] {
        let s = parse_hwkey(spelling).unwrap_or_else(|e| panic!("{spelling}: {e}"));
        assert!(!s.chars.is_empty(), "{spelling} had empty characters");
        assert!(
            !s.chars_ignoring.is_empty(),
            "{spelling} had empty charactersIgnoringModifiers"
        );
    }
}

#[test]
fn pacing_and_modifier_tokens_combine_and_enforce_bounds() {
    let s = parse_hwkey("x count=200 interval=25").unwrap();
    assert_eq!(s.count, 200);
    assert_eq!(s.interval_ms, 25);
    assert_eq!(s.keycode, parse_hwkey("x").unwrap().keycode);

    let combined = parse_hwkey("x mods=shift mods=ctrl").unwrap();
    assert_eq!(combined.flags, FLAG_SHIFT | FLAG_CONTROL);
    assert_eq!(
        parse_hwkey("shift+x mods=ctrl").unwrap().flags,
        FLAG_SHIFT | FLAG_CONTROL
    );

    assert!(parse_hwkey("x count=0").is_err());
    assert!(parse_hwkey(&format!("x count={}", MAX_COUNT + 1)).is_err());
    assert!(parse_hwkey("x count=1oo").is_err());
    assert!(parse_hwkey("x interval=100000").is_err());
    assert!(parse_hwkey("x mods=bogus").is_err());
}

#[test]
fn the_press_shape_is_asked_for_by_name_and_keeps_the_key() {
    let built = parse_hwkey("b mods=alt").unwrap();
    assert!(!built.press);
    let press = parse_hwkey("b mods=alt shape=press").unwrap();
    assert!(press.press);
    assert_eq!((press.keycode, press.flags), (11, super::FLAG_OPTION));
    assert_eq!(press.chars_ignoring, built.chars_ignoring);
    assert_eq!(parse_hwkey("b mods=alt shape=built").unwrap(), built);
    assert!(parse_hwkey("b shape=cg").is_err());
}

#[test]
fn a_malformed_body_is_refused() {
    assert!(parse_hwkey("").is_err());
    assert!(parse_hwkey("   ").is_err());
    assert!(parse_hwkey("notakey").is_err());
    assert!(parse_hwkey("ab").is_err());
    assert!(parse_hwkey("ctrl+").is_err());
}
