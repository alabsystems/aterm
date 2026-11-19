// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the askpass server helpers with an injected clock.

use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use super::*;

#[test]
fn an_unclicked_password_field_ignores_typing_and_return() {
    let input = PasswordInput::new();
    assert!(
        input.key_allowed(false, 0x00),
        "other windows are untouched"
    );
    assert!(input.key_allowed(true, 0x35), "Escape can always cancel");
    for key in [0x00, 0x24, 0x4c, 0x30] {
        assert!(
            !input.key_allowed(true, key),
            "unrelated typing, Return and Tab stay out"
        );
    }
}

#[test]
fn a_clicked_password_field_keeps_normal_keys_and_other_windows() {
    let input = PasswordInput::new();
    assert!(input.key_allowed(false, 0x00));
    assert!(input.key_allowed(true, 0x35));
    input.field_clicked();
    for key in [0x00, 0x24, 0x4c, 0x30] {
        assert!(
            input.key_allowed(true, key),
            "clicked field receives normal keys"
        );
    }
}

#[test]
fn a_touch_marks_the_sheet_as_a_persons_but_admits_no_typing() {
    let input = PasswordInput::new();
    assert!(!input.touched(), "a sheet nobody touched is nobody's yet");
    input.person_touched();
    assert!(input.touched());
    for key in [0x00, 0x24, 0x4c, 0x30] {
        assert!(
            !input.key_allowed(true, key),
            "a click on Details or a key at the window still keeps typing out of the field"
        );
    }
    let clicked = PasswordInput::new();
    clicked.field_clicked();
    assert!(clicked.touched(), "a click into the field is a touch too");
}

#[test]
fn only_a_sheet_nobody_touched_is_refused_and_only_past_its_backstop() {
    let now = Instant::now();
    let raised = now.checked_sub(ASKPASS_UNTOUCHED_BACKSTOP).unwrap();
    assert!(untouched_sheet_overdue(raised, false, now));
    assert!(
        !untouched_sheet_overdue(raised, true, now),
        "a sheet a person touched is theirs until it is answered or withdrawn"
    );
    let inside = raised + Duration::from_secs(1);
    assert!(
        !untouched_sheet_overdue(inside, false, now),
        "inside the backstop it holds"
    );
    assert!(
        !untouched_sheet_overdue(now, false, raised),
        "a clock read before the raise is not past it"
    );
}

#[test]
fn a_password_prompt_refuses_synthetic_input_before_it_is_attached() {
    let gate = PromptInputGate::new();
    let mut posted = Vec::new();
    assert!(gate.with_post(|| posted.push("ordinary input")).is_ok());
    let ((), fence) = gate.present(|| {
        assert!(
            gate.with_post(|| posted.push("password-field input"))
                .is_err(),
            "the secure field can receive events as soon as attachment starts"
        );
    });
    assert_eq!(posted, ["ordinary input"]);
    assert!(gate.with_post(|| posted.push("while visible")).is_err());
    drop(fence);
    assert!(gate.with_post(|| posted.push("after retirement")).is_ok());
    assert_eq!(posted, ["ordinary input", "after retirement"]);
}

#[test]
fn reply_lines_are_shaped_and_hex() {
    let secret = AskpassAnswer::Secret(aterm_uds::sudo::Secret::from_bytes(b"ab"));
    assert_eq!(reply_line(&secret).as_bytes(), b"OK askpass secret=6162\n");
    assert_eq!(
        reply_line(&AskpassAnswer::Cancel).as_bytes(),
        b"ERR askpass declined\n"
    );
    assert_eq!(
        reply_line(&AskpassAnswer::Refused("not-frontmost".into())).as_bytes(),
        b"ERR askpass refused not-frontmost\n"
    );
}

#[test]
fn escape_display_neutralizes_control_and_bidi() {
    assert_eq!(escape_display("rm -rf /a b"), "rm -rf /a b");
    let shown = escape_display("x\ny \u{202e}evil");
    assert!(shown.contains("\\u{000a}"), "{shown}");
    assert!(shown.contains("\\u{202e}"), "{shown}");
    assert!(!shown.contains('\n'), "{shown}");
}

#[test]
fn backoff_widens_on_cancel_and_clears_on_answer() {
    let mut b = Backoff::default();
    let t0 = Instant::now();
    assert!(b.quiet_for(1, t0).is_none(), "clear before any cancel");

    b.on_cancel(1, Some(1), t0, t0);
    let first = b.quiet_for(1, t0).expect("quiet after a cancel");
    assert!(first <= Duration::from_secs(10) && first > Duration::from_secs(9));
    assert!(
        b.quiet_for(2, t0).is_none(),
        "another session is unaffected"
    );

    b.on_cancel(1, Some(1), t0, t0);
    let second = b.quiet_for(1, t0).expect("still quiet");
    assert!(second > first, "{second:?} should exceed {first:?}");

    b.on_answer(1, Some(1));
    assert!(b.quiet_for(1, t0).is_none(), "cleared by an answer");
}

#[test]
fn await_answer_delivers_a_secret() {
    let (tx, rx) = channel();
    tx.send(AskpassAnswer::Secret(aterm_uds::sudo::Secret::from_bytes(
        b"pw",
    )))
    .unwrap();
    let base = Instant::now();
    let deadline = base + Duration::from_secs(300);
    let mut now = || base;
    let mut hung = || false;
    let ticket = AskpassTicket::new(1, deadline);
    let line = await_answer(&rx, &ticket, ASKPASS_TICK, &mut hung, &mut now);
    assert_eq!(line.as_bytes(), b"OK askpass secret=7077\n");
}

#[test]
fn await_answer_times_out_on_the_injected_clock() {
    let (_tx, rx) = channel::<AskpassAnswer>();
    let base = Instant::now();
    let deadline = base + Duration::from_secs(300);
    let mut now = || base + Duration::from_secs(301);
    let mut hung = || false;
    let ticket = AskpassTicket::new(1, deadline);
    let line = await_answer(&rx, &ticket, ASKPASS_TICK, &mut hung, &mut now);
    assert_eq!(line.as_bytes(), b"ERR askpass timeout\n");
}

#[test]
fn await_answer_observes_ticket_cancellation_while_the_sheet_sender_stays_open() {
    for cancel_during_wait in [false, true] {
        let (tx, rx) = channel::<AskpassAnswer>();
        let base = Instant::now();
        let ticket = AskpassTicket::new(1, base + Duration::from_secs(300));
        if !cancel_during_wait {
            ticket.cancel();
        }
        let mut clock_reads = 0;
        let mut now = || {
            clock_reads += 1;
            assert!(
                clock_reads <= 3,
                "a cancelled ticket must end the next poll"
            );
            if cancel_during_wait && clock_reads == 2 {
                ticket.cancel();
            }
            base
        };
        let mut hung = || false;
        let line = await_answer(&rx, &ticket, Duration::ZERO, &mut hung, &mut now);
        assert_eq!(line.as_bytes(), b"ERR askpass declined\n");
        assert!(
            tx.send(AskpassAnswer::Cancel).is_ok(),
            "withdrawal must not depend on the sheet dropping its sender"
        );
    }
}

#[test]
fn a_ticket_cancelled_while_dequeuing_an_answer_does_not_release_a_secret() {
    let (tx, rx) = channel();
    tx.send(AskpassAnswer::Secret(aterm_uds::sudo::Secret::from_bytes(
        b"pw",
    )))
    .unwrap();
    let base = Instant::now();
    let ticket = AskpassTicket::new(1, base + Duration::from_secs(300));
    let mut clock_reads = 0;
    let mut now = || {
        clock_reads += 1;
        if clock_reads == 3 {
            ticket.cancel();
        }
        base
    };
    let line = await_answer(&rx, &ticket, ASKPASS_TICK, &mut || false, &mut now);
    assert_eq!(line.as_bytes(), b"ERR askpass declined\n");
}

#[test]
fn await_answer_gives_up_when_the_caller_hangs_up() {
    let (_tx, rx) = channel::<AskpassAnswer>();
    let base = Instant::now();
    let deadline = base + Duration::from_secs(300);
    let mut now = || base;
    let mut hung = || true;
    let ticket = AskpassTicket::new(1, deadline);
    let line = await_answer(&rx, &ticket, ASKPASS_TICK, &mut hung, &mut now);
    assert_eq!(line.as_bytes(), b"ERR askpass declined\n");
    assert!(
        !ticket.is_live(base),
        "a closed caller must no longer authorize a queued or visible sheet"
    );
}

#[test]
fn an_answer_dequeued_after_its_deadline_does_not_release_a_secret() {
    let (tx, rx) = channel();
    tx.send(AskpassAnswer::Secret(aterm_uds::sudo::Secret::from_bytes(
        b"pw",
    )))
    .unwrap();
    let base = Instant::now();
    let ticket = AskpassTicket::new(2, base + Duration::from_secs(300));
    let mut reads = 0;
    let mut now = || {
        reads += 1;
        if reads == 1 {
            base
        } else {
            base + Duration::from_secs(301)
        }
    };
    let mut hung = || false;
    let line = await_answer(&rx, &ticket, ASKPASS_TICK, &mut hung, &mut now);
    assert_eq!(line.as_bytes(), b"ERR askpass timeout\n");
}

#[test]
fn off_switch_marker_presence_is_the_off_state() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let marker = dir.path().join("askpass-off");
    let off = || super::relay_off_from_metadata(Some(&marker), super::marker_metadata_at);
    assert!(!off(), "default is on (no marker)");
    super::set_marker_at(&marker, true).unwrap();
    assert!(off(), "off when the marker exists");
    super::set_marker_at(&marker, true).unwrap();
    assert!(off(), "repeated off is idempotent");
    super::set_marker_at(&marker, false).unwrap();
    super::set_marker_at(&marker, false).unwrap();
    assert!(!off(), "on again once it is removed");
}

#[cfg(unix)]
#[test]
fn askpass_settings_marker_creation_does_not_write_through_an_existing_entry() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let target = dir.path().join("unrelated-owned-file");
    let marker = dir.path().join("askpass-off");
    std::fs::write(&target, b"private fixture sentinel\n").unwrap();
    std::os::unix::fs::symlink(&target, &marker).unwrap();
    super::set_marker_at(&marker, true).unwrap();
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"private fixture sentinel\n",
        "an existing off entry must not open or truncate its target"
    );
    assert!(super::relay_off_from_metadata(
        Some(&marker),
        super::marker_metadata_at
    ));
    super::set_marker_at(&marker, false).unwrap();
    assert!(!marker.exists());
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"private fixture sentinel\n"
    );
}

#[test]
fn askpass_settings_failed_marker_removal_is_reported() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let marker = dir.path().join("askpass-off");
    std::fs::create_dir(&marker).unwrap();
    assert!(
        super::set_marker_at(&marker, false).is_err(),
        "a marker entry that could not be removed cannot report relay-on success"
    );
    assert!(super::relay_off_from_metadata(
        Some(&marker),
        super::marker_metadata_at
    ));
}

#[test]
fn off_switch_unknown_marker_path_does_not_enable_the_relay() {
    assert!(super::relay_off_from_metadata(None, |_| {
        panic!("a missing state path has no marker path to probe")
    }));
}

#[test]
fn off_switch_metadata_errors_do_not_enable_the_relay() {
    let marker = std::path::Path::new("injected-private-state/askpass-off");
    assert!(!super::relay_off_from_metadata(Some(marker), |_| Err(
        std::io::ErrorKind::NotFound.into()
    )));
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::NotADirectory,
        std::io::ErrorKind::Other,
    ] {
        assert!(
            super::relay_off_from_metadata(Some(marker), |_| Err(kind.into())),
            "uncertain marker state must stay off: {kind:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn off_switch_dangling_marker_is_still_an_off_entry() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let marker = dir.path().join("askpass-off");
    std::os::unix::fs::symlink("missing-owned-marker-target", &marker).unwrap();
    assert!(super::relay_off_from_metadata(
        Some(&marker),
        super::marker_metadata_at
    ));
}

/// A streak eases by itself: each whole [`QUIET_MAX`] a window stood with no sheet after its
/// quiet window ran out takes one end off it, so sheets a program let lapse long ago do not make
/// a person wait five minutes. The quiet time is read to when the sheet was RAISED: a sheet
/// raised as the quiet window ran out that then stood to its deadline (as long as `QUIET_MAX`)
/// widens the streak like any other.
/// NEGATIVE CONTROLS: a second short of `QUIET_MAX` eases nothing; one `QUIET_MAX` eases one
/// step, not the whole streak, so a pause is never the faster way to the next sheet.
#[test]
fn a_streak_eases_one_step_for_each_quiet_max_with_no_sheet() {
    let second = Duration::from_secs(1);
    let mut b = Backoff::default();
    let t0 = Instant::now();
    b.on_cancel(1, Some(1), t0, t0);
    // Raised the moment the quiet window ran out, left to its deadline: the second, 20 s.
    let raised = t0 + QUIET_BASE;
    let ended = raised + ASKPASS_DEADLINE;
    b.on_cancel(1, Some(1), raised, ended);
    assert_eq!(b.quiet_for(1, ended), Some(QUIET_BASE * 2));
    let until = ended + QUIET_BASE * 2;

    // A second short of QUIET_MAX after that: the third, 40 s.
    let near = until + QUIET_MAX - second;
    b.on_cancel(1, Some(1), near, near);
    assert_eq!(b.quiet_for(1, near), Some(QUIET_BASE * 4), "the third");
    let until = near + QUIET_BASE * 4;

    // One QUIET_MAX of quiet takes one end off: 3 - 1 + 1, the third again, not the first.
    let eased = until + QUIET_MAX;
    b.on_cancel(1, Some(1), eased, eased);
    assert_eq!(b.quiet_for(1, eased), Some(QUIET_BASE * 4), "one step");
    let until = eased + QUIET_BASE * 4;

    // Three of them take all three off: the first again.
    let long = until + QUIET_MAX * 3;
    b.on_cancel(1, Some(1), long, long + second);
    assert_eq!(b.quiet_for(1, long + second), Some(QUIET_BASE));

    // However long a streak ran, it eases from the widest quiet window's step.
    let mut b = Backoff::default();
    for _ in 0..40 {
        b.on_cancel(2, Some(2), t0, t0);
    }
    assert_eq!(b.quiet_for(2, t0), Some(QUIET_MAX));
    let later = t0 + QUIET_MAX + QUIET_MAX * 2;
    b.on_cancel(2, Some(2), later, later);
    assert_eq!(b.quiet_for(2, later), Some(QUIET_BASE * 16), "6 - 2 + 1");
}

/// What an update's successor takes up is the streak and the quiet time left, and it goes on
/// from there. NEGATIVE CONTROLS: a quiet window that ran out, a window never spaced and one an
/// answer cleared carry nothing; a row with no streak or no time left spaces nothing.
#[test]
fn a_running_quiet_window_is_carried_and_taken_up_with_its_streak() {
    let secs = Duration::from_secs;
    let mut outgoing = Backoff::default();
    let t0 = Instant::now();
    outgoing.on_cancel(1, Some(1), t0, t0);
    outgoing.on_cancel(1, Some(1), t0, t0);
    assert_eq!(outgoing.carried(1, t0 + secs(5)), Some((2, secs(15))));
    assert_eq!(outgoing.carried(1, t0 + secs(20)), None, "ran out");
    assert_eq!(outgoing.carried(2, t0), None, "never spaced");
    outgoing.on_answer(1, Some(1));
    assert_eq!(outgoing.carried(1, t0), None, "answered");

    let mut successor = Backoff::default();
    let t1 = Instant::now();
    successor.adopt(7, 2, secs(15), t1);
    assert_eq!(successor.quiet_for(7, t1), Some(secs(15)));
    assert_eq!(successor.quiet_for(7, t1 + secs(15)), None);
    successor.on_cancel(7, Some(7), t1 + secs(15), t1 + secs(15));
    assert_eq!(
        successor.quiet_for(7, t1 + secs(15)),
        Some(secs(40)),
        "the third of the streak, not the first"
    );
    successor.adopt(8, 0, secs(15), t1);
    successor.adopt(8, 2, Duration::ZERO, t1);
    assert_eq!(successor.quiet_for(8, t1), None);
}

/// A carried row comes from a file a program of the same user could have written, so it can
/// space a window no longer than that many unanswered sheets would have: the time left is held
/// to the streak's own quiet window and the streak to the one that first reaches the widest.
/// A window already quiet for longer keeps its own. NEGATIVE CONTROL: a shorter own quiet
/// window gives way to the carried one.
#[test]
fn a_carried_spacing_is_held_to_what_its_streak_could_have_set() {
    let secs = Duration::from_secs;
    let mut b = Backoff::default();
    let now = Instant::now();
    b.adopt(1, 1, secs(3600), now);
    assert_eq!(b.quiet_for(1, now), Some(QUIET_BASE));
    b.adopt(2, u32::MAX, secs(3600), now);
    assert_eq!(b.quiet_for(2, now), Some(QUIET_MAX));
    // The held streak widens no further, and does not overflow.
    b.on_cancel(2, Some(2), now, now);
    assert_eq!(b.quiet_for(2, now), Some(QUIET_MAX));

    for _ in 0..3 {
        b.on_cancel(3, Some(3), now, now);
    }
    b.adopt(3, 1, secs(5), now);
    assert_eq!(b.quiet_for(3, now), Some(secs(40)), "its own, longer");
    b.on_cancel(4, Some(4), now, now);
    b.adopt(4, 3, secs(30), now);
    assert_eq!(b.quiet_for(4, now), Some(secs(30)), "the carried, longer");
}

/// A1. A SHEET NOBODY TOUCHED MAKES EVERY WINDOW WAIT, every session too, ones never seen
/// included, so programs that re-ask from two windows cannot keep a sheet standing by turns:
/// window 1's sheet lapses, window 2's request inside that wait is refused, and the sheet window
/// 2 raises the moment the wait runs out widens the same streak, for window 1 as well.
/// NEGATIVE CONTROLS: a refusal that reads only the window and session maps lets window 7 and
/// window 2 through; an unattended feed kept per window spaces only the window that asked.
#[test]
fn a_sheet_nobody_touched_spaces_every_window_and_one_streak_widens_across_them() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    b.on_unattended(t0, t0, 1_000);
    for (window, session) in [(1, 1), (7, 9), (u64::MAX, 42)] {
        assert_eq!(
            b.refusal(window, session, None, t0),
            Some(QUIET_BASE),
            "window {window}, session {session}"
        );
    }
    let asked = t0 + Duration::from_secs(4);
    assert_eq!(
        b.refusal(2, 2, None, asked),
        Some(QUIET_BASE - Duration::from_secs(4)),
        "window 2 asks inside window 1's wait"
    );
    let raised = t0 + QUIET_BASE;
    assert_eq!(b.refusal(2, 2, None, raised), None, "the wait ran out");
    let lapsed = raised + ASKPASS_DEADLINE;
    b.on_unattended(raised, lapsed, 2_000);
    for window in [1, 2, 7] {
        assert_eq!(
            b.refusal(window, window, None, lapsed),
            Some(QUIET_BASE * 2),
            "the second of one streak, for window {window}"
        );
    }
    assert_eq!(
        b.quiet_for(2, lapsed),
        None,
        "no window's own spacing was fed"
    );
}

/// A2. A PERSON THERE AFTER THE SHEET NOBODY TOUCHED IS NOT HELD BY IT: a gesture a millisecond
/// after its end lifts only that instance-wide wait; one at the end itself, or before it, does
/// not; and the window's and the session's own spacing still hold.
/// NEGATIVE CONTROLS: no filter holds the later gesture too; `>=` lets the gesture at the end
/// through.
#[test]
fn only_a_persons_gesture_after_the_unattended_end_lifts_that_wait() {
    let t0 = Instant::now();
    let now = t0 + Duration::from_millis(5);
    let mut b = Backoff::default();
    b.on_cancel(3, Some(30), t0, t0);
    b.on_unattended(t0, t0, 0);
    let left = Some(QUIET_BASE - Duration::from_millis(5));
    let after = Some(t0 + Duration::from_millis(1));
    assert_eq!(b.refusal(4, 40, after, now), None, "a person is there");
    assert_eq!(b.refusal(4, 40, Some(t0), now), left, "at the end itself");
    assert_eq!(
        b.refusal(4, 40, t0.checked_sub(Duration::from_secs(1)), now),
        left,
        "before the end"
    );
    assert_eq!(b.refusal(4, 40, None, now), left, "no person's gesture");
    assert_eq!(b.refusal(3, 40, after, now), left, "the window's own");
    assert_eq!(b.refusal(4, 30, after, now), left, "the session's own");
}

/// A3. AN ANSWER CLEARS ALL THREE for its window and session, and the record goes with the
/// unattended part. NEGATIVE CONTROL: another window's own spacing stays.
#[test]
fn an_answer_clears_the_window_the_session_and_the_unattended_wait() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    b.on_cancel(1, Some(10), t0, t0);
    b.on_cancel(2, Some(20), t0, t0);
    b.on_unattended(t0, t0, 1_000);
    assert!(b.unattended_line(None).is_some());
    b.on_answer(1, Some(10));
    assert_eq!(b.refusal(1, 10, None, t0), None);
    assert_eq!(b.refusal(7, 70, None, t0), None, "no unattended wait");
    assert_eq!(b.unattended_line(None), None, "nothing to record");
    assert_eq!(b.refusal(2, 20, None, t0), Some(QUIET_BASE));
}

/// A4. WHAT A RECORD LEAVES, over a grid of streaks, ends and standing raises: never more than
/// its streak's span, which is at most `QUIET_MAX`; an end or a raise from the future refuses
/// nothing of its own; a raise standing after the end is one more end; and a streak whose quiet
/// ran out eases one step per `QUIET_MAX` since. That standing sheet ended no later than its
/// deadline, so a raise a day old settles into a quiet window long over: a crash under a sheet
/// cannot hold every later launch.
/// NEGATIVE CONTROLS: without the future guard an end an hour ahead refuses; without the
/// standing fold a crash under a sheet adds nothing to the streak; without the deadline bound a
/// raise a day old refuses 300 s.
#[test]
fn a_record_leaves_at_most_its_streaks_span_and_nothing_from_the_future() {
    let q = millis(QUIET_MAX);
    let now: u64 = 1_800_000_000_000;
    let ms = Duration::from_millis;
    for streak in [0, 1, 6, 7, u32::MAX] {
        for ended in [None, Some(0), Some(now - 3 * q), Some(now - 1), Some(now)]
            .into_iter()
            .chain([Some(now + 1), Some(u64::MAX)])
        {
            let standings = [
                None,
                ended.map(|e| e.saturating_sub(1)),
                ended,
                Some(now),
                Some(now + 1),
            ];
            for standing in standings {
                let got = unattended_from_record(streak, ended, standing, now);
                if let Some((s, left, _)) = got {
                    assert!(
                        left <= quiet_span(s.min(STREAK_MAX)) && left <= QUIET_MAX,
                        "{streak} {ended:?} {standing:?}: {got:?}"
                    );
                    assert!(s >= 1, "{got:?}");
                }
                let future = ended.is_some_and(|e| e > now);
                if future && standing.is_none_or(|r| r > now) {
                    assert_eq!(got, None, "{streak} {ended:?} {standing:?}");
                }
                if future && standing == Some(now) {
                    assert_eq!(
                        got,
                        Some((1, QUIET_BASE, Duration::ZERO)),
                        "the raise alone"
                    );
                }
                if standing == Some(now + 1) {
                    assert_eq!(
                        got,
                        unattended_from_record(streak, ended, None, now),
                        "a raise from the future is no raise"
                    );
                }
            }
        }
    }
    assert_eq!(
        unattended_from_record(1, Some(now + 3_600_000), None, now),
        None,
        "an end an hour ahead"
    );
    assert_eq!(
        unattended_from_record(1, Some(now - 1), None, now),
        Some((1, QUIET_BASE - ms(1), ms(1)))
    );
    assert_eq!(
        unattended_from_record(1, Some(now - 1), Some(now), now),
        Some((2, QUIET_BASE * 2, Duration::ZERO)),
        "a sheet standing at the crash is one more end"
    );
    assert_eq!(
        unattended_from_record(0, None, Some(now - 1), now),
        Some((1, QUIET_BASE, Duration::ZERO)),
        "a sheet standing with no streak before it"
    );
    let ran_out = now - millis(quiet_span(6));
    assert_eq!(
        unattended_from_record(6, Some(ran_out - 2 * q), None, now),
        Some((4, Duration::ZERO, ms(millis(quiet_span(6)) + 2 * q)))
    );
    assert_eq!(
        unattended_from_record(6, Some(ran_out - 6 * q), None, now),
        None
    );
    assert_eq!(
        unattended_from_record(6, Some(now - 3 * q), Some(now), now),
        Some((5, quiet_span(5), Duration::ZERO)),
        "a raise two QUIET_MAX after the quiet ran out: 6 - 2 + 1"
    );
    let day: u64 = 86_400_000;
    let deadline = millis(ASKPASS_DEADLINE);
    assert_eq!(
        unattended_from_record(6, Some(now - day - 1), Some(now - day), now),
        None,
        "a raise a day old, at the widest streak"
    );
    assert_eq!(
        unattended_from_record(0, None, Some(now - day), now),
        None,
        "a raise a day old, with no streak before it"
    );
    assert_eq!(
        unattended_from_record(0, None, Some(now - deadline - 100_000), now),
        Some((1, Duration::ZERO, ms(100_000))),
        "a raise past its deadline ended at its deadline: its quiet ran out, its streak is kept"
    );
    assert_eq!(
        settle_record(0, None, Some(now - 2_000), now),
        Settled {
            streak: 1,
            ended: Some(now),
            counted_standing: true,
        },
        "a raise inside its deadline ended no later than the reading"
    );
    assert_eq!(
        settle_record(6, Some(now - 1_000), Some(now), now).streak,
        STREAK_MAX,
        "a settled streak is held as one in memory is"
    );
    assert_eq!(
        settle_record(2, Some(now - 1_000), None, now),
        Settled {
            streak: 2,
            ended: Some(now - 1_000),
            counted_standing: false,
        },
        "no standing raise: the record as it is"
    );
}

/// A5. A TAKE-UP REPLACES THE UNATTENDED WAIT WITH THE RECORD'S, holding its remainder to its
/// streak's span; a record that leaves nothing leaves nothing here; its end age sets the end a
/// person's gesture must follow; and a streak whose quiet ran out is kept, so the next end steps
/// from it. NEGATIVE CONTROL: a take-up that kept the longer wait held here would leave 30 s
/// where the record leaves 5 s, which is how a sheet answered in an update's outgoing process
/// went on spacing every window after it.
#[test]
fn a_take_up_replaces_the_unattended_wait_with_the_records() {
    let secs = Duration::from_secs;
    let now = Instant::now();
    let mut b = Backoff::default();
    b.take_unattended(Some((1, secs(3600), Duration::ZERO)), now, 0);
    assert_eq!(
        b.refusal(9, 9, None, now),
        Some(QUIET_BASE),
        "held to its span"
    );
    b.take_unattended(Some((3, secs(30), Duration::ZERO)), now, 0);
    assert_eq!(b.refusal(9, 9, None, now), Some(secs(30)));
    b.take_unattended(Some((2, secs(5), Duration::ZERO)), now, 0);
    assert_eq!(
        b.refusal(9, 9, None, now),
        Some(secs(5)),
        "the record's, not the longer one held here"
    );
    b.take_unattended(Some((0, secs(300), Duration::ZERO)), now, 0);
    assert_eq!(b.refusal(9, 9, None, now), None, "a streak of 0 is nothing");
    b.take_unattended(Some((2, secs(5), Duration::ZERO)), now, 0);
    b.take_unattended(None, now, 0);
    assert_eq!(b.refusal(9, 9, None, now), None, "no record, no wait");
    assert_eq!(b.unattended_line(None), None);

    let mut b = Backoff::default();
    b.take_unattended(Some((2, secs(15), secs(5))), now, 100_000);
    let ended = now.checked_sub(secs(5)).expect("the clock reaches back");
    assert_eq!(b.refusal(9, 9, Some(ended), now), Some(secs(15)));
    assert_eq!(b.refusal(9, 9, Some(ended + secs(1)), now), None);
    assert_eq!(
        b.unattended_line(None).as_deref(),
        Some("askpass-quiet 1 2 95000 -\n"),
        "the record names when that quiet window started"
    );

    let mut b = Backoff::default();
    b.take_unattended(Some((4, Duration::ZERO, secs(900))), now, 0);
    assert_eq!(b.refusal(9, 9, None, now), None, "its quiet ran out");
    b.on_unattended(now, now, 0);
    assert_eq!(
        b.refusal(9, 9, None, now),
        Some(quiet_span(5)),
        "the fifth of the streak"
    );
}

/// A6. A QUIET REFUSAL NAMES WHOLE SECONDS ROUNDED UP, at least one and at most `QUIET_MAX`.
/// NEGATIVE CONTROL: rounding down names 1 for a wait of 1.5 s.
#[test]
fn a_quiet_refusal_names_its_wait_rounded_up_and_bounded() {
    for (wait, line) in [
        (Duration::from_millis(1500), "ERR askpass refused quiet 2\n"),
        (Duration::ZERO, "ERR askpass refused quiet 1\n"),
        (Duration::from_millis(1), "ERR askpass refused quiet 1\n"),
        (QUIET_BASE, "ERR askpass refused quiet 10\n"),
        (Duration::from_secs(600), "ERR askpass refused quiet 300\n"),
    ] {
        assert_eq!(
            reply_line(&AskpassAnswer::Quiet(wait)).as_bytes(),
            line.as_bytes(),
            "{wait:?}"
        );
    }
    assert_eq!(
        format!("{:?}", AskpassAnswer::Quiet(Duration::from_millis(1500))),
        "Quiet(2s)"
    );
}

/// A7. A SESSION'S OWN WAIT FOLLOWS IT INTO ANOTHER WINDOW (a tab torn off), and a new tab in the
/// old window still waits; a person's Cancel spaces no other session in another window, and a
/// session that retires takes its wait with it. NEGATIVE CONTROL: a refusal without the session
/// map lets the torn-off session through.
#[test]
fn a_sessions_wait_follows_it_into_another_window() {
    let t0 = Instant::now();
    let mut b = Backoff::default();
    b.on_cancel(1, Some(10), t0, t0);
    assert_eq!(b.refusal(2, 10, None, t0), Some(QUIET_BASE), "torn off");
    assert_eq!(b.refusal(1, 11, None, t0), Some(QUIET_BASE), "a new tab");
    assert_eq!(b.refusal(2, 11, None, t0), None, "a person's no is to it");
    assert_eq!(b.carried_session(10, t0), Some((1, QUIET_BASE)));
    b.forget_session(10);
    assert_eq!(b.refusal(2, 10, None, t0), None, "retired");
    let mut successor = Backoff::default();
    successor.adopt_session(5, 2, Duration::from_secs(15), t0);
    assert_eq!(
        successor.refusal(8, 5, None, t0),
        Some(Duration::from_secs(15))
    );
}
