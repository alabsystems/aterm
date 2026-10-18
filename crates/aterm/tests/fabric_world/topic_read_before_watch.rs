// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **AN OPT-IN READ BEFORE ITS SESSION WAS WATCHED IS READ AGAIN.**
//!
//! The bridge learns a session's broadcast opt-ins two ways: read, off `topic
//! ls`, and PUSHED, as the `EVENT <local> topic …` its `@*` push lane drains
//! off the session's timeline. The push lane watches a session only from the
//! wake that ADOPTS it — up to the 250 ms tick after the session registered,
//! or, at aterm's 256-watch cap, only when a slot frees — starting at the
//! timeline's high at that moment. A `topic add` recorded after a read and
//! before the adoption is in neither.
//!
//! The bridge reads before the adoption whenever something other than the
//! session's own watch prompts the read: an attach, a `GAP` or a roster round
//! reads every session the store lists, and a `session-created` reads the one
//! it announces. The endpoint announces and adopts under one guard, and announces a session
//! it could not adopt as `session-created <sid> watch=deferred`. This test
//! reaches that state through the endpoint's debug-only
//! `$ATERM_TEST_PUSH_HOLD`, which holds either decision open while a marker
//! file exists. Nothing here sleeps: every step waits for an observation.
//!
//! What it pins: every `sub <local> <sid>` line is written only after its
//! watch has recorded where it starts, and the bridge reads that session's
//! topics again on it. The 2 s roster round would also learn the opt-in, a
//! round later; `bridge.rs`'s `bridge_rounds_refine_the_announcement_model`
//! pins the ack's read alone, and this test the end-to-end path.

use crate::harness;

use harness::{Hold, World, bridge_holds, until, watchers};

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

    let hold = Hold::new("r26");
    let w = World::boot_with("r26", &[], &[("ATERM_TEST_PUSH_HOLD", &hold.env())]);
    w.wait_ready();
    let s0 = until("aterm's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    });
    // THE BOOT SESSION IS WATCHED before anything is held: it is the one whose
    // pushed record later proves the adoption was handled.
    until("the push lane to watch the boot session", || {
        (watchers(&w, &s0)? >= 1).then_some(())
    });

    // HOLD BOTH DECISIONS, then spawn: the session registers in the store and
    // the push lane neither announces nor adopts it.
    hold.on("adopt");
    hold.on("sessions");
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
    hold.off("sessions");
    until("the bridge to read the new session's opt-ins", || {
        bridge_holds(&w, &s, WARM)
    });
    assert_eq!(
        watchers(&w, &s).unwrap_or_else(|| panic!("`who` lists no {s}")),
        0,
        "the premise: the bridge read {s}'s topics while nothing watched it"
    );

    // THE OPT-IN IN THE WINDOW: read too late for the first sample, recorded
    // before any watch exists to push it.
    topic_add(&w, &s, LATE);
    assert_eq!(
        watchers(&w, &s).unwrap_or_else(|| panic!("`who` lists no {s}")),
        0,
        "the premise: {LATE} was recorded while nothing watched {s}"
    );

    // RELEASE THE ADOPTION, and make the push lane report a record that is
    // ordered after it on the one lane the bridge reads.
    hold.off("adopt");
    topic_add(&w, &s0, WAKE);
    until(
        "the bridge to learn the boot session's pushed opt-in",
        || bridge_holds(&w, &s0, WAKE),
    );
    assert!(
        watchers(&w, &s).is_some_and(|n| n >= 1),
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
}
