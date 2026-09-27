// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 26 — AN OPT-IN READ BEFORE ITS SESSION WAS WATCHED IS READ AGAIN.**
//!
//! The bridge learns a session's broadcast opt-ins two ways: read, off `topic
//! ls`, and PUSHED, as the `EVENT <local> topic …` its `@*` push lane drains
//! off the session's timeline. The push lane only watches a session from the
//! wake that ADOPTS it — up to the 250 ms tick after the session registered,
//! or, at aterm's 256-watch cap, only when a slot frees — and it starts that
//! watch at the timeline's high at that moment. A `topic add` recorded after
//! a read and before the adoption is in neither: the read was too early to
//! see it and the watch starts past it. When this round was written nothing
//! re-read a session's topics on a timer, so such an opt-in was lost for the
//! life of the bridge.
//!
//! The bridge reads before the adoption whenever something other than the
//! session's own watch prompts the read: an attach, a `GAP` or a roster round
//! reads every session the store lists, and a `session-created` reads the one
//! it announces. The endpoint used to announce and adopt in two separate
//! store reads, so a spawn landing between them was announced one wake before
//! it was adopted; it now takes both under one guard, and announces a session
//! it could not adopt as `session-created <sid> watch=deferred`. This test
//! reaches that state through the endpoint's debug-only
//! `$ATERM_TEST_PUSH_HOLD`, which holds either decision open for as long as a
//! marker file exists: with the adoption held, the announcement is the
//! deferred one. Nothing here sleeps: every step waits for an observation.
//!
//! The fix it pins: every `sub <local> <sid>` line — the adoption's here — is
//! written only after its watch has recorded where it starts, and the bridge
//! reads that session's topics again on it. The bridge ALSO reads every
//! listed session it holds no `sub` for on each 2 s roster round, so the
//! opt-in would be learned without the ack's read too, a round later; the
//! bridge's unit tests (`bridge.rs`,
//! `a_session_read_before_its_watch_is_read_again_on_the_watch_ack`) pin the
//! ack's read on its own, and this round pins the end-to-end path.

mod harness;

use harness::{until, World};

/// `Some` once the BRIDGE holds `topic` for `sid`: its persisted cursors, one
/// topic per line.
fn bridge_holds(w: &World, sid: &str, topic: &str) -> Option<()> {
    std::fs::read_to_string(w.state.join(format!("topics/{sid}")))
        .ok()?
        .lines()
        .any(|l| l.split(' ').next() == Some(topic))
        .then_some(())
}

/// How many push-lane watchers aterm counts on `sid` — `who`'s `watchers=`.
fn watchers(w: &World, sid: &str) -> u64 {
    let reply = w.verb("who");
    assert!(reply.ok(), "who: {}", reply.header());
    reply
        .rows()
        .iter()
        .find(|row| row.split_whitespace().nth(1) == Some(sid))
        .and_then(|row| {
            row.split_whitespace()
                .find_map(|t| t.strip_prefix("watchers="))
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or_else(|| panic!("`who` lists no {sid}: {:?}", reply.rows()))
}

/// Opt `sid` into `topic`.
fn topic_add(w: &World, sid: &str, topic: &str) {
    let reply = w.verb(&format!("@{sid} topic add {topic}"));
    assert!(reply.ok(), "@{sid} topic add {topic}: {}", reply.header());
}

/// **A `topic add` MADE BETWEEN THE BRIDGE'S FIRST READ AND THE ADOPTION IS
/// LEARNED.**
///
/// A session is spawned with both of the push lane's decisions held. It opts
/// into [`WARM`], and only then is the announcement released — marked
/// `watch=deferred`, since the adoption is still held — so the bridge's
/// first-sighting read is seen to happen (it learns [`WARM`]) while the
/// session is still unwatched. The session opts into [`LATE`] — no watcher,
/// so nothing can push it — and the adoption is released. A `topic add` on
/// the boot session, which IS watched, wakes the push lane: that wake adopts
/// the new session first, then drains the boot session's timeline, and the
/// bridge reads both off one ordered lane. So once it holds the boot
/// session's topic it has handled the adoption's `sub`, and it must already
/// hold [`LATE`]. Then a record published on [`LATE`] must be delivered.
///
/// A roster round falling between the [`LATE`] add and the release would
/// learn [`LATE`] too — the session holds no `sub` yet, so every round reads
/// it — which can only make this pass early, never fail it.
#[test]
fn a_topic_added_before_the_adoption_is_learned_after_it() {
    const WARM: &str = "r26.warm";
    const LATE: &str = "r26.late";
    const WAKE: &str = "r26.wake";

    let hold = std::env::temp_dir().join(format!("atl-r26-hold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&hold);
    std::fs::create_dir_all(&hold).expect("the hold directory");
    let hold_env = hold.to_string_lossy().into_owned();
    let w = World::boot_with("r26", &[], &[("ATERM_TEST_PUSH_HOLD", &hold_env)]);
    w.wait_ready();
    let s0 = until("aterm's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    });
    // THE BOOT SESSION IS WATCHED before anything is held: it is the one whose
    // pushed record later proves the adoption was handled.
    until("the push lane to watch the boot session", || {
        (watchers(&w, &s0) >= 1).then_some(())
    });

    // HOLD BOTH DECISIONS, then spawn: the session registers in the store and
    // the push lane neither announces nor adopts it.
    std::fs::write(hold.join("adopt"), b"").expect("hold the adoption");
    std::fs::write(hold.join("sessions"), b"").expect("hold the announcement");
    let reply = w.verb("spawn");
    assert!(reply.ok(), "spawn: {}", reply.header());
    let s = reply
        .header()
        .split_whitespace()
        .nth(1)
        .expect("spawn answers OK <sid>")
        .to_string();
    topic_add(&w, &s, WARM);

    // RELEASE THE ANNOUNCEMENT ONLY: `session-created <sid> watch=deferred`.
    // The bridge's `session-created` arm reads the new session's topics (or a
    // roster round does); learning WARM is the observation that a read after
    // the add has happened.
    std::fs::remove_file(hold.join("sessions")).expect("release the announcement");
    until("the bridge to read the new session's opt-ins", || {
        bridge_holds(&w, &s, WARM)
    });
    assert_eq!(
        watchers(&w, &s),
        0,
        "the premise: the bridge read {s}'s topics while nothing watched it"
    );

    // THE OPT-IN IN THE WINDOW: read too late for the first sample, recorded
    // before any watch exists to push it.
    topic_add(&w, &s, LATE);
    assert_eq!(
        watchers(&w, &s),
        0,
        "the premise: {LATE} was recorded while nothing watched {s}"
    );

    // RELEASE THE ADOPTION, and make the push lane report a record that is
    // ordered after it on the one lane the bridge reads.
    std::fs::remove_file(hold.join("adopt")).expect("release the adoption");
    topic_add(&w, &s0, WAKE);
    until(
        "the bridge to learn the boot session's pushed opt-in",
        || bridge_holds(&w, &s0, WAKE),
    );
    assert!(
        watchers(&w, &s) >= 1,
        "the adoption ran before the boot session's record was pushed"
    );
    assert!(
        bridge_holds(&w, &s, LATE).is_some(),
        "the bridge handled {s}'s adoption and still does not hold {LATE}: the \
         opt-in recorded between its first read and the adoption is lost"
    );

    // AND IT IS DELIVERED, end to end.
    let posted = w.verb(&format!("@{s0} post to=say:{LATE} kind=note r26late"));
    assert!(posted.ok(), "{}", posted.header());
    until(
        "the record published on the late opt-in to be delivered",
        || {
            w.inbox(&s)
                .into_iter()
                .find(|r| r.contains(&format!(" topic={LATE} ")) && r.contains("text=r26late"))
        },
    );
    let _ = std::fs::remove_dir_all(&hold);
}
