// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A closed session's subjects go back to its node's subject budget (sim item B02):
//! the bridge's conditional retire, over a real guarded broker retained as aterm's own.

use crate::harness;

use harness::{World, presence_body, until};

/// The sessions spawned beside aterm's boot session.
const SESSIONS: u64 = 4;

/// What the node may still hold past its baseline once the closed sessions' subjects
/// went back: subjects of the node's own faces the test's traffic opened, never one a
/// session held.
const SLACK: u64 = 3;

/// The subjects the node's producer spends, as its `Budget` answers.
fn spent(w: &World) -> u64 {
    w.god()
        .subject_budget(astream_cap::producer_id_of(&w.node))
        .expect("read the node's budget")
        .expect("a broker that reads budgets")
        .subjects
}

/// **A CLOSED SESSION'S SUBJECTS GO BACK TO ITS NODE'S BUDGET**: four sessions open, each
/// mails the next, all close; once the reclaim's wait (shortened to 200 ms) has passed,
/// their presence rows are out of the index, the node spends what it spent before them
/// within [`SLACK`], and a post to one is unroutable. The spend rose by two subjects or
/// more a session while they were open, so the return is not a test that measured
/// nothing.
#[test]
fn a_closed_sessions_subjects_go_back_to_its_nodes_budget() {
    let w = World::boot_with("b02reclaim", &[], &[("ATERM_LINK_RECLAIM_AFTER_MS", "200")]);
    w.wait_ready();
    let first = until("aterm's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    });
    until("the boot session's presence row to say live", || {
        presence_body(&w, &first)
            .filter(|b| b.contains("state=live"))
            .map(|_| ())
    });
    let baseline = spent(&w);

    let sids: Vec<String> = (0..SESSIONS)
        .map(|_| {
            let reply = w.verb("spawn");
            assert!(reply.ok(), "spawn: {}", reply.header());
            reply
                .header()
                .split_whitespace()
                .nth(1)
                .expect("spawn answers OK <sid>")
                .to_string()
        })
        .collect();
    for sid in &sids {
        until(&format!("{sid}'s presence row to say live"), || {
            presence_body(&w, sid)
                .filter(|b| b.contains("state=live"))
                .map(|_| ())
        });
    }
    for (i, from) in sids.iter().enumerate() {
        let to = &sids[(i + 1) % sids.len()];
        w.post_landed(from, to, "note", "", "hello");
    }
    let peak = spent(&w);
    assert!(
        peak >= baseline + 2 * SESSIONS,
        "the sessions spent {} subjects, not their rows and lanes",
        peak - baseline
    );

    for sid in &sids {
        assert!(w.verb(&format!("@{sid} close")).ok(), "close {sid}");
    }
    until("the closed sessions to leave the roster", || {
        let hosted = w.sessions();
        (!hosted.iter().any(|(_, sid, _)| sids.contains(sid))).then_some(())
    });
    // The sweep gives them back one by one: wait for the end, every row out of the index
    // and the spend back, never a budget read between two retires.
    until(
        "the closed sessions' subjects to go back to the node",
        || {
            (sids.iter().all(|sid| presence_body(&w, sid).is_none())
                && spent(&w) <= baseline + SLACK)
                .then_some(())
        },
    );
    let gone = w.verb(&format!(
        "@{first} post to=@{} kind=note --wait=60000 late",
        sids[0]
    ));
    assert!(
        gone.header().starts_with("ERR unroutable"),
        "a post to a session whose row went back: {}",
        gone.header()
    );
    eprintln!(
        "the node spent {baseline} -> {peak} with {SESSIONS} sessions -> {} once they closed",
        spent(&w)
    );
}
