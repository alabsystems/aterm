// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

fn key(event_type: KeyEventType) -> InputEvent {
    InputEvent::Key {
        key: Key::Named(NamedKey::ArrowDown),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type,
    }
}

#[test]
fn the_stamp_reads_never_then_the_time_since() {
    let s = HumanInputStamp::default();
    assert_eq!(s.ms_since(5_000_000), None);
    assert_eq!(s.wire(5_000_000), "-");
    s.note(5_000_000, Gesture::Other);
    assert_eq!(s.ms_since(5_000_000), Some(0));
    assert_eq!(s.ms_since(17_345_678), Some(12_345));
    assert_eq!(s.wire(17_345_678), "12345");
    assert_eq!(s.ms_since(15_000_000), Some(10_000));
    assert_eq!(s.ms_since(4_000_000), Some(0), "a racing read never wraps");
    s.note(0, Gesture::Other);
    assert_eq!(s.ms_since(2_000), Some(2));
}

#[test]
fn a_carried_stamp_reads_its_age_on_until_a_gesture_here() {
    let s = HumanInputStamp::default();
    s.carry(90_000, 5_000_000);
    assert_eq!(s.ms_since(5_000_000), Some(90_000));
    assert_eq!(s.wire(15_000_000), "100000");
    assert_eq!(
        s.ms_since(4_000_000),
        Some(90_000),
        "a racing read never wraps"
    );
    assert_eq!(s.seq(), 0, "the count is this process's");
    s.note(20_000_000, Gesture::Other);
    assert_eq!(s.ms_since(21_000_000), Some(1_000), "the gesture here");
    s.carry(600_000, 21_000_000);
    s.serve_from(600_000, 21_000_000);
    assert_eq!(s.seq(), 1, "the local gesture stays counted");
    assert_eq!(s.ms_since(21_000_000), Some(1_000), "the local stamp wins");
    assert_eq!(s.unkeyed_ms(21_000_000), None, "keyed here");
    assert_eq!(HumanInputStamp::default().ms_since(5_000_000), None);
}

#[test]
fn a_read_that_sees_a_gesture_counted_sees_its_stamp() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    const GESTURES: u64 = 200_000;
    let stamp = Arc::new(HumanInputStamp::default());
    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let stamp = Arc::clone(&stamp);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            for k in 1..=GESTURES {
                stamp.note(k * 1000, Gesture::Other);
            }
            done.store(true, Ordering::Release);
        })
    };
    let (mut reads, mut torn) = (0_u64, 0_u64);
    while !done.load(Ordering::Acquire) {
        let n = stamp.seq();
        let at = stamp.last();
        if n > 0 && at < n * 1000 + 1 {
            torn += 1;
        }
        reads += 1;
    }
    writer.join().expect("the writer");
    assert_eq!(
        torn, 0,
        "{torn} of {reads} reads paired a counted gesture with an older stamp"
    );
    assert_eq!(stamp.seq(), GESTURES);
    assert_eq!(stamp.last(), GESTURES * 1000 + 1);
}

#[test]
fn a_persons_burst_opens_after_the_gap_without_input() {
    let gap_us = crate::session_timeline::HUMAN_BURST_GAP_MS * 1000;
    let s = HumanInputStamp::default();
    assert_eq!(s.wire(5_000_000), "-");
    assert!(
        s.note(5_000_000, Gesture::Other),
        "the first gesture opens a burst"
    );
    assert!(!s.note(5_400_000, Gesture::Other), "inside the burst");
    assert!(
        !s.note(5_400_000 + gap_us - 1_000, Gesture::Other),
        "a gesture just inside the gap after the last is inside it"
    );
    let mut t = 5_400_000 + gap_us - 1_000;
    for _ in 0..200 {
        t += 3_000_000;
        assert!(!s.note(t, Gesture::Other), "at {t}");
    }
    assert!(
        s.note(t + gap_us, Gesture::Other),
        "a full gap without input opens the next"
    );
    assert_eq!(s.ms_since(t + gap_us + 101_000), Some(101));
    let early = HumanInputStamp::default();
    assert!(early.note(0, Gesture::Other));
    assert!(!early.note(3_000, Gesture::Other));
}

#[test]
fn the_seam_stamps_a_persons_arrow_and_never_a_controllers() {
    use crate::input::Source;
    use crate::{App, WindowId};
    let mut app = App::headless_for_test();
    let stamp = |app: &App| {
        app.pool
            .get(0)
            .expect("session 0")
            .ctx
            .human_input
            .ms_since(crate::metrics::now_us())
    };
    assert_eq!(stamp(&app), None, "a fresh session: never");
    let _ = app.input(WindowId(0), key(KeyEventType::Press), Source::Controller);
    assert_eq!(stamp(&app), None, "a control verb's key is no person's");
    let _ = app.input(WindowId(0), key(KeyEventType::Release), Source::Human);
    let _ = app.input(WindowId(0), InputEvent::Focus(true), Source::Human);
    assert_eq!(
        stamp(&app),
        None,
        "a release and a focus change are no gesture"
    );
    let _ = app.input(WindowId(0), key(KeyEventType::Press), Source::Human);
    assert!(
        stamp(&app).is_some_and(|ms| ms < 60_000),
        "the person's arrow stamped the session: {:?}",
        stamp(&app)
    );
}

/// THE PERSON'S STAMPS CROSS A SEAMLESS UPDATE (LC-4 of ):
/// the outgoing process writes, for the sessions it hands over, the age
/// of each one's last person's gesture — a stamp it carried itself from
/// an earlier update included — and for a session nobody keyed, how long
/// it served it so (`-<age>`, the review's KR-2), and how long ago the
/// reaper's look last saw a program in a concluded worker's tab (`j<id>`, the
/// 2026-11-02 review); the successor's Commit carries each onto the shell
/// adopted under the id it names and consumes the sidecar.
#[cfg(unix)]
#[test]
fn the_persons_stamps_cross_a_seamless_update_at_its_commit() {
    use crate::App;
    let dir = crate::control_auth::socket_dir().expect("a private control dir");
    let path = dir.join(format!(
        "seamless-{}-{}.person",
        std::process::id(),
        aterm_uds::rand::hex_token::<8>().expect("a token")
    ));
    let mut outgoing = App::headless_for_test();
    let keyed = *outgoing.pool.sessions.keys().max().expect("session 0");
    let _ = outgoing.insert_logical_window(crate::stub_session(1), 24, 80);
    let untouched = *outgoing
        .pool
        .sessions
        .keys()
        .max()
        .expect("a second session");
    assert_ne!(keyed, untouched);
    let now = crate::metrics::now_us();
    outgoing
        .pool
        .get(keyed)
        .expect("live")
        .ctx
        .human_input
        .carry(90_000, now);
    outgoing
        .pool
        .get(keyed)
        .expect("live")
        .ctx
        .human_input
        .carry_typed(80_000, now);
    outgoing
        .pool
        .get(keyed)
        .expect("live")
        .ctx
        .human_input
        .carry_typed_awake(70_000, now);
    outgoing
        .pool
        .get(untouched)
        .expect("live")
        .ctx
        .human_input
        .serve_from(600_000, now);
    let program_seen = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(50))
        .expect("up 50 s");
    outgoing.worker_job_seen.insert(untouched, program_seen);
    let written = outgoing
        .write_person_stamps_of(&[keyed, untouched], path.clone())
        .expect("written");
    assert_eq!(written, path);
    let body = std::fs::read_to_string(&path).expect("the sidecar");
    let rows: Vec<&str> = body.lines().collect();
    assert!(
        rows.len() == 6
            && rows[0].starts_with(&format!("{keyed} 9"))
            && rows[1].starts_with(&format!("t{keyed} 8"))
            && rows[2].starts_with(&format!("a{keyed} 7"))
            && rows[3].starts_with(&format!("{untouched} -6"))
            && rows[4] == format!("t{untouched} -")
            && rows[5].starts_with(&format!("j{untouched} 5")),
        "the keyed age, its typing's (and in awake time), the unkeyed one and the claim \
         look's last sight of a program: {body:?}"
    );

    let read = |carried: Option<std::path::PathBuf>| {
        let mut successor = App::headless_for_test();
        let _ = successor.insert_logical_window(crate::stub_session(1), 24, 80);
        // Each adopted shell is named by the other's outgoing id (both apps number theirs
        // 0 and 1), so a stamp carried under the named id, not the adopted one, shows.
        let mut ids: Vec<u64> = successor.pool.sessions.keys().copied().collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            [keyed, untouched],
            "PRECONDITION: the same ids on both sides"
        );
        for (&id, named) in ids.iter().zip([untouched, keyed]) {
            let session = successor.pool.sessions.get_mut(&id).expect("live");
            session.session.handoff_local_id = Some(named);
            session
                .session
                .ctx
                .human_input
                .serve_from(3_600_000, crate::metrics::now_us());
        }
        successor.handoff_person_stamps = carried;
        successor.take_carried_person_stamps();
        let now = crate::metrics::now_us();
        ids.iter()
            .map(|&id| {
                let stamp = &successor.pool.get(id).expect("live").ctx.human_input;
                (
                    stamp.ms_since(now),
                    stamp.unkeyed_ms(now),
                    stamp.typed_ms_since(now),
                    stamp.typed_awake_ms(now),
                    successor
                        .worker_job_seen
                        .get(&id)
                        .map(|seen| seen.elapsed().as_millis()),
                )
            })
            .collect::<Vec<_>>()
    };
    let carried = read(Some(path.clone()));
    assert!(
        matches!(
            carried[..],
            [
                (None, Some(served), None, None, Some(seen)),
                (Some(keyed), None, Some(typed), Some(awake), None),
            ]
                if keyed >= 90_000 && typed >= 80_000 && (70_000..80_000).contains(&awake)
                    && served >= 600_000 && (50_000..3_600_000).contains(&seen)
        ),
        "(adopted as untouched, adopted as keyed): {carried:?}"
    );
    assert!(!path.exists(), "consumed");
    let none = read(Some(path.clone()));
    assert!(
        matches!(
            none[..],
            [(None, Some(a), None, None, None), (None, Some(b), None, None, None)]
                if a < 60_000 && b < 60_000
        ),
        "NEGATIVE CONTROL: no sidecar, never keyed and served since the Commit: {none:?}"
    );
}

/// THE PERSON'S TYPING AGE COUNTS THE MAC'S SLEEP (2026-10-30 review 9): `human_typed_ms=`
/// times the harness's hour after the person's key, and the window's clock stops while the
/// Mac sleeps. Typed, then asleep for fifteen hours, the age reads those hours; carried across
/// a seamless update, the age the outgoing process wrote counts the sleep after it too. With
/// no sleep, or no clock that says it, the age is the awake time, as `human_ms=`'s is.
#[test]
fn the_typing_age_counts_the_macs_sleep() {
    const HOURS: u64 = 15 * 3600;
    let s = HumanInputStamp::default();
    s.stamp_typed(1_000_000, Some(5_000_000));
    assert_eq!(
        s.typed_ms_at(4_000_000, Some(5_000_000)),
        Some(3_000),
        "awake"
    );
    assert_eq!(
        s.typed_ms_at(4_000_000, Some(5_000_000 + HOURS * 1_000_000)),
        Some(3_000 + HOURS * 1_000),
        "through the sleep"
    );
    assert_eq!(
        s.typed_ms_at(4_000_000, None),
        Some(3_000),
        "no sleep clock"
    );
    let carried = HumanInputStamp::default();
    carried.carry_typed_at(80_000, 1_000_000, Some(5_000_000));
    assert_eq!(
        carried.typed_ms_at(2_000_000, Some(5_000_000 + HOURS * 1_000_000)),
        Some(81_000 + HOURS * 1_000),
        "carried, then asleep"
    );
    let now = crate::metrics::now_us();
    let typed = HumanInputStamp::default();
    typed.note(now, Gesture::Typed);
    assert_eq!(
        typed.typed_ms_since(now),
        typed.ms_since(now),
        "no sleep since: the gesture's own age"
    );
}

/// THE AWAKE TYPING AGE LEAVES THE SLEEP OUT, A CARRIED ONE TOO (2026-11-02 review): a claim
/// worker's task record crosses a seamless update on the awake clock, so the typing age the
/// look holds it against may not count the sleep before the update that the carried age
/// does. Stamped here, the awake time since; carried with its awake row, that age; carried
/// with only the age that counts sleep (an older writer), the carried last gesture's awake age
/// where it is sooner, which no typing before the update is later than, and never a click
/// made here, which is no typing. CONTROL: no typing, none.
#[test]
fn the_awake_typing_age_leaves_the_sleep_out() {
    const SLEPT_MS: u64 = 8 * 3600 * 1000;
    let here = HumanInputStamp::default();
    here.stamp_typed(1_000_000, Some(5_000_000));
    assert_eq!(here.typed_awake_ms(4_000_000), Some(3_000));

    let older = HumanInputStamp::default();
    older.carry(30_000, 1_000_000);
    older.carry_typed_at(60_000 + SLEPT_MS, 1_000_000, None);
    assert_eq!(
        older.typed_ms_at(2_000_000, None),
        Some(61_000 + SLEPT_MS),
        "PRECONDITION: the carried age counts the sleep before the update"
    );
    assert_eq!(
        older.typed_awake_ms(2_000_000),
        Some(31_000),
        "an older writer: the last gesture's awake age"
    );
    older.note(1_800_000, Gesture::Other);
    assert_eq!(
        older.typed_awake_ms(2_000_000),
        Some(31_000),
        "a click made here before any typing here is no typing"
    );
    older.carry_typed_awake(60_000, 1_000_000);
    assert_eq!(
        older.typed_awake_ms(2_000_000),
        Some(61_000),
        "the awake row"
    );
    older.stamp_typed(1_500_000, None);
    assert_eq!(
        older.typed_awake_ms(2_000_000),
        Some(500),
        "typed here since"
    );

    let legacy = HumanInputStamp::default();
    legacy.carry_typed_at(45_000, 1_000_000, None);
    assert_eq!(
        legacy.typed_awake_ms(2_000_000),
        Some(46_000),
        "a possible key with no gesture carried beside it"
    );
    assert_eq!(HumanInputStamp::default().typed_awake_ms(2_000_000), None);
}

/// An older sidecar has no separate typing provenance. Its recent gesture may be a key,
/// so adoption keeps that age conservatively. A modern pointing-only or untouched session
/// must still carry no typing, and a modern typing age must survive normally.
#[cfg(unix)]
#[test]
fn legacy_stamps_keep_a_possible_key_and_modern_pointing_keeps_none() {
    use crate::App;
    let dir = crate::control_auth::socket_dir().expect("a private control dir");
    for kind in ["typed", "pointed", "unkeyed", "legacy"] {
        let outgoing = App::headless_for_test();
        let named = *outgoing.pool.sessions.keys().max().expect("session 0");
        let stamp = &outgoing.pool.get(named).expect("live").ctx.human_input;
        let now = crate::metrics::now_us();
        if kind == "unkeyed" {
            stamp.serve_from(600_000, now);
        } else {
            stamp.carry(90_000, now);
            if kind == "typed" {
                stamp.carry_typed(90_000, now);
            }
        }
        let path = dir.join(format!(
            "seamless-{}-{}.person",
            std::process::id(),
            aterm_uds::rand::hex_token::<8>().expect("a token")
        ));
        if kind == "legacy" {
            assert!(crate::seamless::write_person_stamps(
                &path,
                &[(named, crate::seamless::PersonStamp::Keyed(90_000))],
            ));
        } else {
            assert_eq!(
                outgoing.write_person_stamps_of(&[named], path.clone()),
                Some(path.clone())
            );
        }
        let mut successor = App::headless_for_test();
        let adopted = *successor.pool.sessions.keys().max().expect("session 0");
        successor
            .pool
            .sessions
            .get_mut(&adopted)
            .expect("live")
            .session
            .handoff_local_id = Some(named);
        successor.handoff_person_stamps = Some(path.clone());
        successor.take_carried_person_stamps();
        let stamp = &successor.pool.get(adopted).expect("live").ctx.human_input;
        let now = crate::metrics::now_us();
        let (human, typed) = (stamp.ms_since(now), stamp.typed_ms_since(now));
        assert!(!path.exists(), "the {kind} fixture was consumed");
        if kind == "unkeyed" {
            assert_eq!((human, typed), (None, None));
            assert!(stamp.unkeyed_ms(now).is_some_and(|age| age >= 600_000));
        } else {
            assert!(human.is_some_and(|age| age >= 90_000), "{kind}: {human:?}");
            if kind == "pointed" {
                assert_eq!(typed, None, "a modern pointer is no key");
            } else {
                assert!(
                    typed.is_some_and(|age| (90_000..3_600_000).contains(&age)),
                    "{kind}: the recent possible key must keep its age: {typed:?}"
                );
            }
        }
    }
}

#[test]
fn a_never_keyed_session_reads_how_long_it_was_served() {
    let s = HumanInputStamp::default();
    let now = crate::metrics::now_us();
    assert!(
        s.unkeyed_ms(now).is_some_and(|ms| ms < 60_000),
        "since its start"
    );
    s.serve_from(600_000, 5_000_000);
    assert_eq!(s.unkeyed_ms(5_000_000), Some(600_000));
    assert_eq!(s.unkeyed_ms(15_000_000), Some(610_000));
    s.serve_from(0, 5_000_000);
    assert_eq!(
        s.unkeyed_ms(4_000_000),
        Some(0),
        "a racing read never wraps"
    );
    let carried = HumanInputStamp::default();
    carried.carry(90_000, 5_000_000);
    assert_eq!(carried.unkeyed_ms(5_000_000), None, "keyed, carried");
    s.note(6_000_000, Gesture::Other);
    assert_eq!(s.unkeyed_ms(7_000_000), None, "keyed here");
}

fn enter(mods: Modifiers, event_type: KeyEventType) -> InputEvent {
    InputEvent::Key {
        key: Key::Named(NamedKey::Enter),
        mods,
        base_layout: None,
        layout_text: None,
        event_type,
    }
}

#[test]
fn only_a_persons_plain_enter_press_marks_the_count() {
    use crate::input::{PasteFraming, Source};
    use crate::{App, WindowId};
    let mut app = App::headless_for_test();
    let probe = |app: &App| app.pool.get(0).expect("session 0").ctx.human_input.probe();
    let at = |seq, plain_enter| PersonProbe { seq, plain_enter };
    let plain = enter(Modifiers::empty(), KeyEventType::Press);
    let letter = InputEvent::Key {
        key: Key::Character('a'),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    assert_eq!(probe(&app), at(0, false), "a fresh session");
    let _ = app.input(WindowId(0), plain.clone(), Source::Controller);
    assert_eq!(probe(&app), at(0, false), "a control verb's Enter");
    let _ = app.input(WindowId(0), plain.clone(), Source::Human);
    assert_eq!(probe(&app), at(1, true), "the person's Enter");
    let mut seq = 1;
    for (what, lock) in [
        ("Caps Lock", Modifiers::CAPS_LOCK),
        ("Num Lock", Modifiers::NUM_LOCK),
    ] {
        let _ = app.input(WindowId(0), letter.clone(), Source::Human);
        let _ = app.input(WindowId(0), enter(lock, KeyEventType::Press), Source::Human);
        seq += 2;
        assert_eq!(probe(&app), at(seq, true), "Enter with {what} on");
    }
    let others = [
        ("Shift+Enter", enter(Modifiers::SHIFT, KeyEventType::Press)),
        (
            "Shift+Enter with Caps Lock on",
            enter(Modifiers::SHIFT | Modifiers::CAPS_LOCK, KeyEventType::Press),
        ),
        ("a repeat", enter(Modifiers::empty(), KeyEventType::Repeat)),
        (
            "keypad Enter",
            InputEvent::Key {
                key: Key::Named(NamedKey::NumpadEnter),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
        ),
        ("text", InputEvent::Text("\r".into())),
        (
            "a paste",
            InputEvent::Paste("claude\r".into(), PasteFraming::AtDrain),
        ),
        ("a chord", InputEvent::KeySequence(b"\r".to_vec())),
        ("a letter", letter),
    ];
    for (what, ev) in others {
        let _ = app.input(WindowId(0), plain.clone(), Source::Human);
        assert_eq!(probe(&app), at(seq + 1, true), "the Enter before {what}");
        let _ = app.input(WindowId(0), ev, Source::Human);
        seq += 2;
        assert_eq!(probe(&app), at(seq, false), "{what}");
    }
}

#[test]
fn navigation_counts_and_releases_hovers_focus_and_geometry_do_not() {
    assert!(is_person_gesture(&key(KeyEventType::Press)));
    assert!(is_person_gesture(&key(KeyEventType::Repeat)));
    assert!(!is_person_gesture(&key(KeyEventType::Release)));
    assert!(is_person_gesture(&InputEvent::Text("1".into())));
    assert!(is_person_gesture(&InputEvent::KeySequence(
        b"\x1b[B".to_vec()
    )));
    assert!(is_person_gesture(&InputEvent::Paste(
        "x".into(),
        crate::input::PasteFraming::AtDrain
    )));
    assert!(!is_person_gesture(&InputEvent::Focus(true)));
    assert!(!is_person_gesture(&InputEvent::Resize {
        rows: 24,
        cols: 80,
        echo_to_window: false
    }));
    assert!(!is_person_gesture(&InputEvent::ResizeWindowPx {
        width: 800,
        height: 600
    }));
}

#[test]
fn a_reading_that_sees_a_gesture_reach_the_program_counts_it() {
    let s = HumanInputStamp::default();
    s.note_reaching(1_000, Gesture::Other, false);
    let mut inside = PersonCounts::default();
    let noted = s.note_reaching_split(2_000, Gesture::Other, true, || inside = s.counts());
    assert_eq!(noted.seq, 2);
    let counts = |seq, reached, burst| PersonCounts {
        seq,
        reached,
        burst,
    };
    assert_eq!(inside, counts(2, 0, 1), "counted first");
    assert_eq!(s.counts(), counts(2, 2, 1));
    s.reached.fetch_max(3, Ordering::Release);
    assert_eq!(s.counts(), counts(3, 3, 1));
    let t = HumanInputStamp::default();
    t.note_reaching(1_000, Gesture::Other, false);
    let late = 1_000 + crate::session_timeline::HUMAN_BURST_GAP_MS * 1000;
    let noted = t.note_reaching_split(late, Gesture::Other, true, || inside = t.counts());
    assert!(noted.burst, "{noted:?}");
    assert_eq!(inside, counts(2, 0, 1), "counted, neither published");
    assert_eq!(t.counts(), counts(2, 2, 2));
}

#[test]
fn a_drag_reaches_the_program_only_where_its_mode_reports_motion() {
    use crate::input::PixelOffset;
    use aterm_core::selection::SelectionSide;
    let drag = InputEvent::MouseMove {
        buttons: 0,
        row: 1,
        col: 4,
        mods: 0,
        side: SelectionSide::Left,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let press = InputEvent::MouseButton {
        button: aterm_types::mouse::MouseButton::Left,
        pressed: true,
        row: 1,
        col: 4,
        mods: 0,
        click_count: 1,
        side: SelectionSide::Left,
        block: false,
        suppress_copy_on_select: false,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    for (set, motion) in [
        (&b""[..], false),
        (b"\x1b[?9h", false),
        (b"\x1b[?1000h", false),
        (b"\x1b[?1002h", true),
        (b"\x1b[?1003h", true),
    ] {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        term.process(set);
        let mirror = term.mode_mirror();
        let mode = String::from_utf8_lossy(set);
        assert_eq!(
            reaches_program(&drag, mirror, crate::input::WheelPlatform::Host),
            motion,
            "a drag under {mode:?}"
        );
        assert_eq!(
            term.encode_mouse_motion(0, 4, 1, 0).is_some(),
            motion,
            "the encoder under {mode:?}"
        );
        assert_eq!(
            reaches_program(&press, mirror, crate::input::WheelPlatform::Host),
            !set.is_empty(),
            "a press under {mode:?}"
        );
    }
}
