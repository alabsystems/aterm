// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const S: Duration = Duration::from_secs(1);

#[test]
fn a_spinners_frames_are_one_line_and_its_job_line_progress_once() {
    let mut c = Console::new();
    // systemd's spinner: a carriage return, an erase, the frame, colours, a moving
    // asterisk run and a growing elapsed time.
    let frames = [
        "\r\u{1b}[K[\u{1b}[0;1;31m*     \u{1b}[0m] A start job is running for Wait for Network to be Configured (2s / no limit)",
        "\r\u{1b}[K[\u{1b}[0;1;31m**    \u{1b}[0m] A start job is running for Wait for Network to be Configured (3s / no limit)",
        "\r\u{1b}[K[ \u{1b}[0;31m***  \u{1b}[0m] A start job is running for Wait for Network to be Configured (1min 59s / no limit)",
    ];
    c.feed(frames[0].as_bytes(), S);
    c.feed(b"\r", S);
    assert_eq!(c.last_progress(), Some(S));
    for (i, frame) in frames.iter().enumerate().skip(1) {
        let at = S * (10 + u32::try_from(i).unwrap());
        c.feed(frame.as_bytes(), at);
        c.feed(b"\r", at);
    }
    assert_eq!(c.last_progress(), Some(S), "a spinner frame is no progress");
    assert_eq!(c.counts(), (3, 2));
    c.feed(b"[  OK  ] Reached target network.target.\n", S * 30);
    assert_eq!(c.last_progress(), Some(S * 30), "a new line is");
}

#[test]
fn the_negative_control_distinct_lines_are_each_progress() {
    let mut c = Console::new();
    for i in 0..5u32 {
        let line = format!(
            "[  OK  ] Started unit-{}.service.\n",
            char::from(b'a' + i as u8)
        );
        c.feed(line.as_bytes(), S * i);
        assert_eq!(c.last_progress(), Some(S * i));
    }
    assert_eq!(c.counts(), (5, 0));

    let mut bounded = Console::new();
    for i in 0..(SEEN_LINES + 10) {
        let word = format!(
            "{}{}{}\n",
            char::from(b'a' + u8::try_from(i % 26).unwrap()),
            char::from(b'a' + u8::try_from((i / 26) % 26).unwrap()),
            char::from(b'a' + u8::try_from((i / 676) % 26).unwrap()),
        );
        bounded.feed(word.as_bytes(), S);
    }
    assert_eq!(bounded.seen.len(), SEEN_LINES);
    assert_eq!(bounded.seen_order.len(), SEEN_LINES);
}

#[test]
fn a_kernel_panic_is_seen_once_with_its_line() {
    let mut c = Console::new();
    assert!(
        c.feed(b"[    1.2] Run /init as init process\n", S)
            .is_empty()
    );
    let seen = c.feed(
        b"[    1.9] Kernel panic - not syncing: VFS: Unable to mount root fs on unknown-block(0,0)\n",
        S,
    );
    assert_eq!(
        seen,
        [Seen::Panic(
            "[    1.9] Kernel panic - not syncing: VFS: Unable to mount root fs on unknown-block(0,0)"
                .into()
        )]
    );
    assert!(c.feed(b"Kernel panic - not syncing: again\n", S).is_empty());
    assert!(c.tail().contains("Unable to mount root fs"));
}

#[test]
fn the_tail_keeps_the_last_lines_each_capped_and_a_partial_line_waits() {
    let mut c = Console::new();
    for i in 0..(TAIL_LINES + 5) {
        c.feed(format!("line {i} of {}\n", "x".repeat(i)).as_bytes(), S);
    }
    let tail = c.tail();
    assert_eq!(tail.lines().count(), TAIL_LINES);
    assert!(tail.starts_with("line 5 of"), "{tail}");
    c.feed(&b"y".repeat(LINE_BYTES * 2), S);
    assert_eq!(
        c.tail(),
        tail,
        "a line without its end is not in the tail yet"
    );
    c.feed(b"\n", S);
    let last = c.tail().lines().last().unwrap().to_string();
    assert!(
        last.ends_with('…') && last.len() <= LINE_BYTES + 3,
        "{}",
        last.len()
    );
}

#[test]
fn normalizing_drops_escapes_digits_and_spinner_glyphs() {
    assert_eq!(
        normalized("\u{1b}[0;1;31m[ ***] \u{1b}[0mJob x.service/start running (12s / 1min 30s)"),
        "[ ] Job x.service/start running ( / )"
    );
    assert_eq!(normalized("   \u{1b}[K  "), "");
    // A word that only starts like a unit stays.
    assert_eq!(normalized("took 3 seconds, 4 days"), "took seconds, days");
    assert_eq!(normalized("eth0 up after 1min 2s"), "eth up after");
    assert_eq!(normalized("4é5mé"), "émé");
}
