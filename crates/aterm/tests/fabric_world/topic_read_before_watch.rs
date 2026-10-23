// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration test verifying that topic opt-ins recorded before watch adoption are re-read.

use crate::harness;

use harness::{Hold, World, bridge_holds, until, watchers};

/// Opt `sid` into `topic`.
fn topic_add(w: &World, sid: &str, topic: &str) {
    let reply = w.verb(&format!("@{sid} topic add {topic}"));
    assert!(reply.ok(), "@{sid} topic add {topic}: {}", reply.header());
}

/// **A `topic add` MADE BETWEEN THE BRIDGE'S FIRST READ AND THE ADOPTION IS
/// LEARNED.**
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
    until("the push lane to watch the boot session", || {
        (watchers(&w, &s0)? >= 1).then_some(())
    });

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

    hold.off("sessions");
    until("the bridge to read the new session's opt-ins", || {
        bridge_holds(&w, &s, WARM)
    });
    assert_eq!(
        watchers(&w, &s).unwrap_or_else(|| panic!("`who` lists no {s}")),
        0,
        "the premise: the bridge read {s}'s topics while nothing watched it"
    );

    topic_add(&w, &s, LATE);
    assert_eq!(
        watchers(&w, &s).unwrap_or_else(|| panic!("`who` lists no {s}")),
        0,
        "the premise: {LATE} was recorded while nothing watched {s}"
    );

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
