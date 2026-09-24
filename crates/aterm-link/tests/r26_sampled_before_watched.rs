// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 26 — AN OPT-IN READ BEFORE ITS SESSION WAS WATCHED IS READ AGAIN.**
//!
//! The bridge learns a session's broadcast opt-ins two ways: once, off
//! `topic ls`, when it first learns of the session, and from then on PUSHED,
//! as the `EVENT <local> topic …` its `@*` push lane drains off the session's
//! timeline. The push lane only watches a session from the wake that ADOPTS it
//! — up to the 250 ms tick after the session registered — and it starts that
//! watch at the timeline's high at that moment. A `topic add` recorded after
//! the bridge's read and before the adoption is in neither: the read was too
//! early to see it and the watch starts past it. Nothing re-reads a session's
//! topics on a timer, so the opt-in was lost for the life of the bridge.
//!
//! The bridge can read before the adoption in two ways: an attach or a `GAP`
//! re-reads every session the store lists, adopted or not; and the endpoint
//! announces a session (`session-created`) and adopts it with two separate
//! store reads, so a spawn landing between them is announced one wake before
//! it is adopted. This test forces the second through the endpoint's
//! debug-only `$ATERM_TEST_PUSH_HOLD`, which holds either read open for as long
//! as a marker file exists. Nothing here sleeps: every step waits for an
//! observation.
//!
//! The fix it pins: the adoption's own `sub <local> <sid>` line is written
//! only after the watch has recorded where it starts, so a bridge that has
//! already read that session's topics reads them again on it.

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
/// A session is spawned with both of the push lane's reads held. It opts into
/// [`WARM`], and only then is the announcement released, so the bridge's
/// first-sighting read is seen to happen (it learns [`WARM`]) while the
/// session is still unwatched. The session opts into [`LATE`] — no watcher, so
/// nothing can push it — and the adoption is released. A `topic add` on the
/// boot session, which IS watched, wakes the push lane: that wake adopts the
/// new session first, then drains the boot session's timeline, and the bridge
/// reads both off one ordered lane. So once it holds the boot session's topic
/// it has handled the adoption, and it must already hold [`LATE`]. Then a
/// record published on [`LATE`] must be delivered.
///
/// Without the re-read on `sub`, the last two assertions fail at once and for
/// good: no attach, no `GAP` and no later `session-created` happens in this
/// world, and nothing else reads the session's topics again.
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

    // HOLD BOTH READS, then spawn: the session registers in the store and the
    // push lane neither announces nor adopts it.
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

    // RELEASE THE ANNOUNCEMENT ONLY. The bridge's `session-created` arm reads
    // the new session's topics; learning WARM is the observation that it has.
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
