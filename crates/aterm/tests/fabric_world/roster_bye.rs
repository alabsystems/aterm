// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Roster session withdrawal on `bye quit` vs preservation on `bye handoff` (design §6).

use crate::harness;

use harness::{FLEET, Fleet, Node, until};

/// Every `state=` a presence face has carried, oldest first: a withdrawal is
/// a RECORD, and the newest row alone would not show one that came and went.
fn states(fleet: &Fleet, face: &str) -> Vec<String> {
    let (rows, _) = fleet
        .read(|c| c.fetch(0, face, 256))
        .unwrap_or_else(|e| panic!("fetch {face}: {e}"));
    rows.iter()
        .filter(|(_, s, _)| s == face)
        .filter_map(|(_, _, b)| {
            let b = String::from_utf8_lossy(b);
            b.split_whitespace()
                .find_map(|t| t.strip_prefix("state="))
                .map(str::to_string)
        })
        .collect()
}

/// Two nodes up, each with its boot session advertised `live`, and A's first
/// post to B's session landed — so A's bridge has PINNED B's sid, the state a
/// sender is in when the session it has been talking to goes away.
fn talking(fleet: &mut Fleet, tag: &str, b_env: &[(&str, &str)]) -> (Node, Node, String, String) {
    let a = fleet.node(&format!("{tag}-a"), &[]);
    let b = fleet.node_with(&format!("{tag}-b"), &[], &[], b_env);
    a.wait_ready(fleet);
    b.wait_ready(fleet);
    let (sid_a, _) = a.session();
    let (sid_b, _) = b.session();
    let face = format!("/f/{FLEET}/pub/{}/{sid_b}/presence", b.node);
    until("B's session to be advertised live", || {
        fleet.row(&face).filter(|r| r.contains("state=live"))
    });
    let first = a.verb(&format!("@{sid_a} post to=@{sid_b} kind=ask --wait first"));
    assert!(
        first.ok() && first.header().contains("off="),
        "the first post must land: {} / {}",
        first.header(),
        a.log_tail()
    );
    (a, b, sid_a, sid_b)
}

/// **A QUIT WITHDRAWS ITS SESSIONS, AND A POST TO ONE IS REFUSED AT THE
/// SENDER.** B quits: its bridge publishes `state=exited` for B's session and
/// says so in its `ev`, B's node reads `away` once its bridge has gone, and
/// A — which routed to that sid a moment ago and pinned it — is told
/// `ERR unroutable` by its own bridge instead of `OK off=` for a lane nobody
/// will fetch.
#[test]
fn a_quit_withdraws_its_sessions_and_a_post_to_one_is_refused() {
    let mut fleet = Fleet::boot("byeq");
    let (a, mut b, sid_a, sid_b) = talking(&mut fleet, "byeq", &[]);
    let face = format!("/f/{FLEET}/pub/{}/{sid_b}/presence", b.node);

    b.quit();
    until("the quit to withdraw B's session", || {
        (states(&fleet, &face).last().map(String::as_str) == Some("exited")).then_some(())
    });
    let quits: Vec<String> = b
        .ev(&fleet)
        .into_iter()
        .filter(|e| e.starts_with("bye reason=quit"))
        .collect();
    assert_eq!(
        quits,
        ["bye reason=quit withdrawn=1"],
        "the withdrawal says what it was"
    );
    let node = format!("/f/{FLEET}/pub/{}/node/presence", b.node);
    until("B's node to read away once its bridge is gone", || {
        fleet.row(&node).filter(|r| r.contains("state=away"))
    });

    let refused = a.verb(&format!("@{sid_a} post to=@{sid_b} kind=ask --wait second"));
    assert!(
        refused.header().starts_with("ERR unroutable"),
        "a post to a withdrawn session must be refused at the sender: {}",
        refused.header()
    );
    let named = a.verb(&format!(
        "@{sid_a} post to=@{sid_b}@{} kind=ask --wait third",
        b.node
    ));
    assert!(
        named.header().starts_with("ERR unroutable"),
        "and so must one that names the node that withdrew it: {}",
        named.header()
    );
}

/// **A HANDOFF WITHDRAWS NOTHING, AND A POST TO AN AWAY SESSION QUEUES.** B
/// says `bye handoff` and its lanes close: its bridge takes the word (its
/// `ev`), its connection ends and the node's will reads `away` — and B's
/// session row was never withdrawn, because the successor would adopt that
/// sid. A's post to it is not refused: it LANDS on B's lane on the bus
/// (`OK off=`), waiting for the node to come back.
#[test]
fn a_handoff_withdraws_nothing_and_a_post_to_an_away_session_queues() {
    let mut fleet = Fleet::boot("byeh");
    let (a, mut b, sid_a, sid_b) =
        talking(&mut fleet, "byeh", &[("ATERM_DEBUG_QUIT_BYE", "handoff")]);
    let face = format!("/f/{FLEET}/pub/{}/{sid_b}/presence", b.node);

    b.quit();
    until("B's bridge to take the handoff", || {
        b.ev(&fleet)
            .iter()
            .any(|e| e == "bye reason=handoff")
            .then_some(())
    });
    let node = format!("/f/{FLEET}/pub/{}/node/presence", b.node);
    until("B's node to read away once its bridge is gone", || {
        fleet.row(&node).filter(|r| r.contains("state=away"))
    });
    let history = states(&fleet, &face);
    assert!(
        !history.iter().any(|s| s == "exited"),
        "a handoff withdrew B's session: {history:?}"
    );
    assert_eq!(history.last().map(String::as_str), Some("live"));

    let queued = a.verb(&format!("@{sid_a} post to=@{sid_b} kind=ask --wait queued"));
    assert!(
        queued.ok() && queued.header().contains("off="),
        "a post to an away session must land on the bus for its node: {} / {}",
        queued.header(),
        a.log_tail()
    );
    let lane = format!("/f/{FLEET}/in/{}/{sid_b}/{}/ask", b.node, a.node);
    let (rows, _) = fleet
        .read(|c| c.fetch(0, &lane, 16))
        .unwrap_or_else(|e| panic!("fetch {lane}: {e}"));
    assert!(
        rows.iter()
            .any(|(_, _, body)| String::from_utf8_lossy(body).contains("queued")),
        "the post waits on B's lane: {rows:?}"
    );
}
