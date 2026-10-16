// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the askpass server helpers — behaviour only, no timing budgets
//! (the clock is injected).

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

/// The reply line for a secret is `OK askpass secret=<hex>`, and for a cancel
/// / refusal the matching `ERR` — and a secret's reply never contains the raw
/// bytes.
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

/// The command display escapes control, newline and bidi characters, so a
/// crafted command line cannot inject rows into the sheet or reorder what the
/// owner reads. Ordinary text is untouched.
#[test]
fn escape_display_neutralizes_control_and_bidi() {
    assert_eq!(escape_display("rm -rf /a b"), "rm -rf /a b");
    let shown = escape_display("x\ny \u{202e}evil");
    assert!(shown.contains("\\u{000a}"), "{shown}");
    assert!(shown.contains("\\u{202e}"), "{shown}");
    assert!(!shown.contains('\n'), "{shown}");
}

/// Backoff: a session is clear until it cancels; then it is quiet for a
/// widening window; an answer clears it. (Positive control: a different session
/// is unaffected.)
#[test]
fn backoff_widens_on_cancel_and_clears_on_answer() {
    let mut b = Backoff::default();
    let t0 = Instant::now();
    assert!(b.quiet_for(1, t0).is_none(), "clear before any cancel");

    b.on_cancel(1, t0);
    let first = b.quiet_for(1, t0).expect("quiet after a cancel");
    assert!(first <= Duration::from_secs(10) && first > Duration::from_secs(9));
    assert!(
        b.quiet_for(2, t0).is_none(),
        "another session is unaffected"
    );

    // A second consecutive cancel widens the window past the first.
    b.on_cancel(1, t0);
    let second = b.quiet_for(1, t0).expect("still quiet");
    assert!(second > first, "{second:?} should exceed {first:?}");

    // An answer clears it.
    b.on_answer(1);
    assert!(b.quiet_for(1, t0).is_none(), "cleared by an answer");
}

/// await_answer returns the secret's line when the sheet answers before the
/// deadline.
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

/// await_answer returns the timeout line once the deadline passes with no
/// answer — proven with a clock the test advances, not by waiting.
#[test]
fn await_answer_times_out_on_the_injected_clock() {
    let (_tx, rx) = channel::<AskpassAnswer>();
    let base = Instant::now();
    let deadline = base + Duration::from_secs(300);
    // The clock jumps past the deadline on the first read.
    let mut now = || base + Duration::from_secs(301);
    let mut hung = || false;
    let ticket = AskpassTicket::new(1, deadline);
    let line = await_answer(&rx, &ticket, ASKPASS_TICK, &mut hung, &mut now);
    assert_eq!(line.as_bytes(), b"ERR askpass timeout\n");
}

/// await_answer treats a hung-up caller as a decline, promptly.
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

/// The owner off-switch: absent marker means on (the default); its presence
/// means off. Tested with an explicit path in a tempdir so it never touches the
/// real state dir or races the `ATERM_STATE_HOME` env.
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
