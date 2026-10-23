// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE: THE HEADLESS CONTROL-SOCKET SMOKE — a real `aterm --headless`, one
//! `cursor` round trip, then a human-shaped typing burst over the control
//! socket, and one counter off `metrics`, on the suite's own boot (it waits on
//! the instance's `--ready-fd`, never polls for its socket).
//!
//! Two checks, each the only automatic guard of its failure:
//!
//! * `cursor` answers `OK <row> <col> <visible:0|1> <style>[ frame=open]`: the preflight every
//!   `aterm drive` verb sends first, through the `aterm ctl` client, and no other
//!   test sends it. The shape, not a bare `OK *`, so a verb routed to another
//!   handler that also answers `OK …` does not pass for it.
//! * `wake_heals` stays 0 over a burst of which at least half the keys were
//!   delivered. A heal is a `Wake::Output` whose latch is still armed after a
//!   user-event drain that began after its post — the lost-wake class, which
//!   the watchdog turns from a permanent 5 fps into one hiccup per lost wake.
//!   A burst whose every wake is lost and healed can still clear a window's
//!   frames floor, so this counter is the one reading that sees the class.
//!
//! Every wait is for its event: the instance's own ready word (`--ready-fd`,
//! `support/headless_boot.rs`), the screen going quiet before the burst (so a
//! startup-edge repair is never counted as the burst's) and after it (so the
//! last echo's wake has been judged), each a server-side `await idle` under the
//! hang detector. The 50 ms between keys is the burst's SHAPE — about twenty
//! keys a second, a person typing — never a wait.

use std::time::Duration;

use crate::headless_boot::{self, ctl_ok};
use crate::live_ctl::{AWAIT_MS, Ctl};

/// The burst: 30 keys about 20 a second, a person's light typing, deliberately
/// not a socket flood.
const BURST_KEYS: usize = 30;
const BURST_GAP: Duration = Duration::from_millis(50);

/// Keys the burst must deliver ([`is_applied_send`]) before `wake_heals=0`
/// means anything: each delivered key is one echo and one wake, so a burst
/// that delivered only a few gave a lost wake only a few chances.
const ACCEPTED_FLOOR: usize = BURST_KEYS / 2;

/// How long the screen holds still before and after the burst: past the
/// output watchdog's own expiry (a frame or two plus 100 ms), so the wake of
/// the last echo has been judged — healed or not — before `metrics` is read.
const QUIET_MS: u64 = 500;

/// `cursor`'s documented reply (aterm-gui's `control_query::cmd_cursor`), ` frame=open` ending one
/// read on a DEC 2026 frame still unfinished at its bound.
fn is_cursor_reply(reply: &str) -> bool {
    let fields: Vec<&str> = reply.split_whitespace().collect();
    matches!(
        fields.as_slice(),
        ["OK", row, col, "0" | "1", _style] | ["OK", row, col, "0" | "1", _style, "frame=open"]
            if row.parse::<u32>().is_ok() && col.parse::<u32>().is_ok()
    )
}

/// An applied `send` answers `OK`, with the dispatcher adding its sequence
/// (`OK seq=<n>`). Additive warning fields still describe a write; a skipped
/// or duplicate answer delivered no new input, so it is not a delivered key.
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

/// Wait (server-side, the hang detector) for the screen to hold still
/// [`QUIET_MS`]; `when` names the moment for the failure.
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

    // The `aterm drive` preflight, through the client every verb uses.
    let cursor = ctl_ok(&inst, &["cursor"]);
    assert!(
        is_cursor_reply(cursor.trim()),
        "`aterm ctl cursor` answered {cursor:?}"
    );

    let mut ctl = Ctl::connect(&inst.sock);
    // The shell's start drawn and the reader lane at its ordinary idle wait,
    // so a startup-edge repair is not the burst's.
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

/// NEGATIVE CONTROL for the cursor shape: a bare `OK`, a reply with a word
/// where a number goes, and a visibility other than 0 or 1 are not `cursor`'s.
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

/// NEGATIVE CONTROL for the delivered-key count: a skipped or duplicate
/// answer, a malformed sequence and anything not `OK` deliver nothing.
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
