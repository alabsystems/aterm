// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless control-socket smoke test: verifies the `cursor` preflight reply shape and that
//! `wake_heals` stays `0` across a 20-key/s typing burst.

use std::time::Duration;

use crate::headless_boot::{self, ctl_ok};
use crate::live_ctl::{AWAIT_MS, Ctl};

/// Burst size (30 keys at 50 ms intervals).
const BURST_KEYS: usize = 30;
const BURST_GAP: Duration = Duration::from_millis(50);

/// Minimum delivered keys ([`is_applied_send`]) required for `wake_heals=0` to be non-vacuous.
const ACCEPTED_FLOOR: usize = BURST_KEYS / 2;

/// Quiet window before and after the burst, past the output watchdog expiry.
const QUIET_MS: u64 = 500;

/// Whether `reply` matches `cursor`'s `OK <row> <col> <0|1> <style>[ frame=open]` shape.
fn is_cursor_reply(reply: &str) -> bool {
    let fields: Vec<&str> = reply.split_whitespace().collect();
    matches!(
        fields.as_slice(),
        ["OK", row, col, "0" | "1", _style] | ["OK", row, col, "0" | "1", _style, "frame=open"]
            if row.parse::<u32>().is_ok() && col.parse::<u32>().is_ok()
    )
}

/// Whether `reply` is an applied `send` receipt (`OK` or `OK seq=<n>` without `skipped`/`dup=1`).
fn is_applied_send(reply: &str) -> bool {
    if reply == "OK" {
        return true;
    }
    let Some(rest) = reply.strip_prefix("OK seq=") else {
        return false;
    };
    let mut fields = rest.split_ascii_whitespace();
    fields.next().is_some_and(|seq| seq.parse::<u64>().is_ok())
        && fields.all(|field| field != "skipped" && field != "dup=1")
}

/// Wait for the screen to hold still for [`QUIET_MS`].
#[track_caller]
fn quiet(ctl: &mut Ctl, when: &str) {
    let reply = ctl.ok(&format!("await idle {QUIET_MS} timeout={AWAIT_MS}"));
    assert!(
        !reply.starts_with("OK timeout"),
        "the screen never held still {QUIET_MS} ms {when}: {reply}"
    );
}

/// `key=<u64>` out of one reply line.
fn metric(reply: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key}=");
    reply
        .split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))?
        .parse()
        .ok()
}

#[test]
fn a_cursor_round_trip_and_a_typing_burst_heal_no_lost_wake() {
    let inst = headless_boot::boot("atsmk", &["--lines", "24", "--columns", "80"]);

    let cursor = ctl_ok(&inst, &["cursor"]);
    assert!(
        is_cursor_reply(cursor.trim()),
        "`aterm ctl cursor` answered {cursor:?}"
    );

    let mut ctl = Ctl::connect(&inst.sock);
    quiet(&mut ctl, "before the burst");
    ctl.ok("metrics reset");
    let mut accepted = 0usize;
    for _ in 0..BURST_KEYS {
        if is_applied_send(&ctl.line("send x")) {
            accepted += 1;
        }
        std::thread::sleep(BURST_GAP);
    }
    assert!(
        accepted >= ACCEPTED_FLOOR,
        "only {accepted}/{BURST_KEYS} keys accepted (< {ACCEPTED_FLOOR}): too few for a lost \
         wake to register"
    );
    quiet(&mut ctl, "after the burst");
    let metrics = ctl.ok("metrics");
    assert_eq!(
        metric(&metrics, "wake_heals"),
        Some(0),
        "{accepted}/{BURST_KEYS} keys accepted; a lost output wake was healed during a plain \
         typing burst: {metrics}"
    );
}

/// Negative control for [`is_cursor_reply`] and [`metric`].
#[test]
fn only_cursors_own_reply_reads_as_one() {
    assert!(is_cursor_reply("OK 3 7 1 block"));
    assert!(is_cursor_reply("OK 3 7 1 block frame=open"));
    for not in [
        "OK",
        "OK 3 7 2 block",
        "OK row 7 1 block",
        "OK 3 7 1",
        "ERR 3 7 1 x",
        "OK 3 7 1 block frame=shut",
        "OK 3 7 1 block frame=open x",
    ] {
        assert!(!is_cursor_reply(not), "{not}");
    }
    assert_eq!(metric("OK a=1 wake_heals=0 b=2", "wake_heals"), Some(0));
    assert_eq!(metric("OK xwake_heals=3", "wake_heals"), None);
}

/// Negative control for [`is_applied_send`]: skipped, duplicate, or malformed replies do not count.
#[test]
fn only_applied_send_receipts_count_as_delivered_keys() {
    for applied in [
        "OK",
        "OK seq=0",
        "OK seq=18446744073709551615",
        "OK seq=7 warn=input-unread input_bytes=3 input_wait_ms=1500",
    ] {
        assert!(is_applied_send(applied), "{applied}");
    }
    for refused in [
        "",
        "OKAY",
        "OK skipped",
        "OK skipped reason=changed seq=7",
        "OK dup=1 seq=7",
        "OK seq=7 skipped",
        "OK seq=7 dup=1",
        "ERR busy sink",
        "OK seq=",
        "OK seq=18446744073709551616",
        "OK seq=7extra",
        "aterm: note\nOK seq=7",
    ] {
        assert!(!is_applied_send(refused), "{refused}");
    }
}
