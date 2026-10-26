// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The reclaim (`Bridge::reclaim_subjects`) against a real broker: what a departed
// session held goes back to the node's subject budget, and what routes to it after.

use super::*;

const A: &str = "s-a";
const B: &str = "s-b";

/// [`advertising`], its connection having said `Hello` as an attach's has: the reclaim
/// needs the broker's retire verb and the log it serves.
fn attached(
    sessions: &[(u64, &str)],
) -> (
    FakeVerbLane,
    Bridge,
    FakePushLane,
    Conn,
    astream_broker::BrokerHandle,
) {
    let (lane, mut bridge, push, reader, server) = advertising(sessions);
    assert!(
        bridge.read_log_identity().unwrap().is_some(),
        "no log identity"
    );
    assert!(bridge.conn.as_ref().is_some_and(|c| c.supports_retire()));
    (lane, bridge, push, reader, server)
}

/// Every subject the reclaim tracks has stood for its whole wait.
fn wait_passed(bridge: &mut Bridge) {
    let past = Instant::now()
        .checked_sub(reclaim_after())
        .expect("an instant one reclaim wait ago (the host booted since)");
    for (_, since) in bridge.reclaim.values_mut() {
        *since = past;
    }
}

/// The subjects the node's producer spends, as its `Budget` answers.
fn spent(bridge: &Bridge, reader: &mut Conn) -> u64 {
    reader
        .subject_budget(bridge.producer_id)
        .unwrap()
        .expect("a broker that reads budgets")
        .subjects
}

/// `subject`'s row in the last-value index, if it has one.
fn indexed(reader: &mut Conn, subject: &str) -> Option<String> {
    let (rows, _) = reader.last(subject, "", 4).unwrap();
    rows.into_iter()
        .find(|(_, s, _)| s == subject)
        .map(|(_, _, raw)| String::from_utf8_lossy(&raw).into_owned())
}

/// A mails B and B mails A, then B leaves: aterm drops it from the roster and says so on
/// the push lane (`session-exited`), as a closed tab does.
fn mailed_then_b_left(
    lane: &FakeVerbLane,
    bridge: &mut Bridge,
    push: &mut FakePushLane,
    reader: &mut Conn,
) -> (String, String, u64) {
    *lane.outbox.lock().unwrap() = Some(
        "post sid=s-a id=1 to=@s-b kind=note len=2\nhi\
         post sid=s-b id=1 to=@s-a kind=note len=2\nho"
            .to_string(),
    );
    bridge.drain_outbox();
    let to_a = format!("/f/f/in/{0}/{A}/{0}/note", bridge.node);
    let to_b = format!("/f/f/in/{0}/{B}/{0}/note", bridge.node);
    for lane in [&to_a, &to_b] {
        assert!(indexed(reader, lane).is_some(), "{lane} was not written");
    }
    let before = spent(bridge, reader);
    lane.sessions.lock().unwrap().retain(|(_, sid)| sid != B);
    push.deliver(bridge, &[&format!("EVENT 2 session-exited {B}")]);
    bridge.settle_acks();
    assert_eq!(presence_states(bridge, reader, B), ["live", "exited"]);
    (to_a, to_b, before)
}

/// **A DEPARTED SESSION'S LANE AND ROW GO BACK, AND NOTHING ROUTES TO IT AFTER**: the
/// sweep that first finds B gone gives nothing back; once its wait has passed, the lane
/// to B and B's presence row leave the index, the budget is two subjects lighter, and B's
/// pin goes, so a bare `@s-b` and an explicit `@s-b@<node>` are both unroutable. Nothing
/// of A's goes. A pin written again past the retire finds no route either: the row is
/// gone.
#[cfg(unix)]
#[test]
fn a_departed_sessions_lane_and_row_go_back_and_nothing_routes_to_it_after() {
    let (lane, mut bridge, mut push, mut reader, _server) = attached(&[(1, A), (2, B)]);
    let (to_a, to_b, before) = mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    let node = bridge.node.clone();
    bridge.state.pin(B, &node).unwrap();

    bridge.retire_ghost_presence();
    assert_eq!(
        spent(&bridge, &mut reader),
        before,
        "nothing goes before its wait"
    );
    assert!(bridge.reclaim.contains_key(&to_b), "{:?}", bridge.reclaim);
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();

    assert_eq!(spent(&bridge, &mut reader), before - 2);
    let b_row = subject::session_face("f", &node, B, "presence");
    assert_eq!(indexed(&mut reader, &to_b), None);
    assert_eq!(indexed(&mut reader, &b_row), None);
    assert!(indexed(&mut reader, &to_a).is_some(), "a lane to A went");
    let a_row = indexed(
        &mut reader,
        &subject::session_face("f", &node, A, "presence"),
    );
    assert!(
        a_row.is_some_and(|row| row.contains("state=live")),
        "A's row went"
    );
    assert!(bridge.reclaim.is_empty(), "{:?}", bridge.reclaim);
    assert!(!bridge.state.pins().contains_key(B), "B's pin stayed");
    assert_eq!(
        bridge.resolve_to("@s-b", Some(A)).unwrap(),
        Route::Unroutable
    );
    assert_eq!(
        bridge.resolve_to(&format!("@s-b@{node}"), Some(A)).unwrap(),
        Route::Unroutable
    );
    assert_eq!(
        bridge.resolve_to("@s-a", Some(A)).unwrap(),
        Route::To(format!("/f/f/in/{node}/{A}/{node}"))
    );

    bridge.retire_ghost_presence();
    assert_eq!(
        spent(&bridge, &mut reader),
        before - 2,
        "a later sweep is idle"
    );

    bridge.state.pin(B, &node).unwrap();
    assert_eq!(
        bridge.resolve_to("@s-b", Some(A)).unwrap(),
        Route::Unroutable,
        "a pin to a node that advertises no row for the sid routed"
    );
}

/// **A SESSION THAT COMES BACK IS ADVERTISED AGAIN AND ROUTES**: after B's subjects went
/// back, aterm serves B again; its row is written anew and both addresses route to it.
#[cfg(unix)]
#[test]
fn a_session_back_after_its_subjects_went_is_advertised_again_and_routes() {
    let (lane, mut bridge, mut push, mut reader, _server) = attached(&[(1, A), (2, B)]);
    let (_, _, before) = mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    bridge.retire_ghost_presence();
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(spent(&bridge, &mut reader), before - 2);

    lane.sessions.lock().unwrap().push((3, B.to_string()));
    bridge.refresh_sessions().unwrap();
    bridge.admit_fresh_sessions();
    bridge.settle_acks();
    let node = bridge.node.clone();
    let row = indexed(
        &mut reader,
        &subject::session_face("f", &node, B, "presence"),
    );
    assert!(
        row.is_some_and(|row| row.contains("state=live")),
        "B was not advertised again"
    );
    for to in ["@s-b".to_string(), format!("@s-b@{node}")] {
        assert_eq!(
            bridge.resolve_to(&to, Some(A)).unwrap(),
            Route::To(format!("/f/f/in/{node}/{B}/{node}")),
            "{to}"
        );
    }
}

/// **A SESSION WITH A POST STILL RESERVED KEEPS ITS SUBJECTS**: B left with a post of its
/// own reserved (a reserve record of the post journal), so nothing of B's goes; once the
/// post is retired, both go.
#[cfg(unix)]
#[test]
fn a_departed_session_with_a_post_still_reserved_keeps_its_subjects() {
    let (lane, mut bridge, mut push, mut reader, _server) = attached(&[(1, A), (2, B)]);
    let (_, _, before) = mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    bridge.state.set_post_seq(B, 9, 99, false).unwrap();
    bridge.retire_ghost_presence();
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(
        spent(&bridge, &mut reader),
        before,
        "a post of B's is still owed"
    );
    assert!(!bridge.reclaim.is_empty());

    bridge.state.clear_post_seq(B, 9);
    bridge.retire_ghost_presence();
    assert_eq!(spent(&bridge, &mut reader), before - 2);
}

/// Another node, `n-..fa`, its node row live and `s-x` live on it, written by its own
/// producer: the connection that writes its rows, and the subject of `s-x`'s row.
fn far_node_with_s_x(bridge: &Bridge) -> (Conn, String) {
    let (mut peer, _) = bridge.connect().unwrap();
    peer.publish(
        FAR_PRODUCER,
        1,
        &subject::node_face("f", FAR, "presence"),
        b"v=1 t=1 state=live inc=1",
    )
    .unwrap();
    let x_row = subject::session_face("f", FAR, "s-x", "presence");
    peer.publish(FAR_PRODUCER, 2, &x_row, b"v=1 t=1 inc=1 state=live")
        .unwrap();
    (peer, x_row)
}

const FAR: &str = "n-00000000000000fa";
const FAR_PRODUCER: u64 = 9_000;

/// A's post to `to`, drained: the lane it lands on, which this node writes to `s-x`.
fn a_posts_to(lane: &FakeVerbLane, bridge: &mut Bridge, reader: &mut Conn, to: &str) -> String {
    *lane.outbox.lock().unwrap() = Some(format!("post sid=s-a id=7 to={to} kind=note len=2\nhi"));
    bridge.drain_outbox();
    bridge.settle_acks();
    let to_x = format!("/f/f/in/{FAR}/s-x/{}/note", bridge.node);
    assert!(
        indexed(reader, &to_x).is_some(),
        "the lane to s-x was not written"
    );
    to_x
}

/// **A LANE TO ANOTHER NODE'S DEPARTED SESSION GOES BACK, WITH NOTHING ON THIS NODE TO
/// SAY IT LEFT**: A mails `s-x` on another node, whose row then reads `exited`. That is
/// no event here (no session of this node left, no attach), yet the lane A wrote keeps the
/// sweep looking: the sweeps that follow find `s-x` gone, and once the wait has passed the
/// lane, which only this node is charged for, leaves the index. Nothing of the other
/// node's goes, and with no lane to another node left the sweep is owed nothing more.
#[cfg(unix)]
#[test]
fn a_lane_to_another_nodes_departed_session_goes_back() {
    let (lane, mut bridge, _push, mut reader, _server) = attached(&[(1, A)]);
    let (mut peer, x_row) = far_node_with_s_x(&bridge);
    assert!(
        !bridge.reclaim_remote,
        "a lane to another node before any post"
    );
    let to_x = a_posts_to(&lane, &mut bridge, &mut reader, &format!("@s-x@{FAR}"));
    assert!(
        bridge.reclaim_remote && bridge.sweep_owed(),
        "a lane to another node's session owes no sweep"
    );
    let before = spent(&bridge, &mut reader);

    bridge.retire_ghost_presence();
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(
        spent(&bridge, &mut reader),
        before,
        "s-x is live on its node: its lane stays"
    );
    assert!(bridge.sweep_owed(), "the sweep stopped looking at s-x");

    peer.publish(FAR_PRODUCER, 3, &x_row, b"v=1 t=2 inc=1 state=exited")
        .unwrap();
    assert!(
        !bridge.reclaim_owed,
        "the far session's leaving was an event here"
    );
    bridge.retire_ghost_presence();
    assert!(bridge.reclaim.contains_key(&to_x), "{:?}", bridge.reclaim);
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(spent(&bridge, &mut reader), before - 1);
    assert_eq!(indexed(&mut reader, &to_x), None);
    assert!(
        indexed(&mut reader, &x_row).is_some(),
        "the other node's row went"
    );
    assert!(!bridge.reclaim_remote, "{:?}", bridge.reclaim);
    assert!(bridge.reclaim.is_empty(), "{:?}", bridge.reclaim);
}

/// **A SESSION WHOSE NODE GAVE ITS ROW BACK IS UNROUTABLE FROM ANOTHER NODE**: A posts
/// to a bare `@s-x`, which pins `s-x` to its node. `s-x` leaves, and its node's own
/// reclaim gives its row back. Neither the explicit `@s-x@<node>` nor the pinned bare
/// `@s-x` routes then, before this node's sweep or after it; this node's sweep gives its
/// lane back and drops the pin. An address naming that node and a sid it never had is no
/// route either. CONTROL: `s-x` live again on its node routes both forms.
#[cfg(unix)]
#[test]
fn a_session_whose_node_gave_its_row_back_is_unroutable_from_another_node() {
    let (lane, mut bridge, _push, mut reader, _server) = attached(&[(1, A)]);
    let (mut peer, x_row) = far_node_with_s_x(&bridge);
    let to_x = a_posts_to(&lane, &mut bridge, &mut reader, "@s-x");
    assert_eq!(
        bridge.state.pins().get("s-x").map(String::as_str),
        Some(FAR)
    );
    let explicit = format!("@s-x@{FAR}");
    let lane_to_x = Route::To(format!("/f/f/in/{FAR}/s-x/{}", bridge.node));
    for to in ["@s-x", explicit.as_str()] {
        assert_eq!(bridge.resolve_to(to, Some(A)).unwrap(), lane_to_x, "{to}");
    }
    assert_eq!(
        bridge
            .resolve_to(&format!("@s-never@{FAR}"), Some(A))
            .unwrap(),
        Route::Unroutable,
        "a sid the named node never advertised"
    );

    peer.publish(FAR_PRODUCER, 3, &x_row, b"v=1 t=2 inc=1 state=exited")
        .unwrap();
    for to in ["@s-x", explicit.as_str()] {
        assert_eq!(
            bridge.resolve_to(to, Some(A)).unwrap(),
            Route::Unroutable,
            "{to}"
        );
    }
    let (_, moot) = peer
        .retire_subject(FAR_PRODUCER, &x_row)
        .expect("the far node gives s-x's row back");
    assert!(!moot, "the far row's retire was moot");
    assert_eq!(indexed(&mut reader, &x_row), None);
    for to in ["@s-x", explicit.as_str()] {
        assert_eq!(
            bridge.resolve_to(to, Some(A)).unwrap(),
            Route::Unroutable,
            "{to} routed once its node gave the row back"
        );
    }

    bridge.retire_ghost_presence();
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(indexed(&mut reader, &to_x), None, "the lane to s-x stayed");
    assert_eq!(bridge.state.pins().get("s-x"), None, "s-x's pin stayed");
    for to in ["@s-x", explicit.as_str()] {
        assert_eq!(
            bridge.resolve_to(to, Some(A)).unwrap(),
            Route::Unroutable,
            "{to} routed after this node's sweep"
        );
    }

    peer.publish(FAR_PRODUCER, 4, &x_row, b"v=1 t=3 inc=2 state=live")
        .unwrap();
    for to in ["@s-x", explicit.as_str()] {
        assert_eq!(
            bridge.resolve_to(to, Some(A)).unwrap(),
            lane_to_x,
            "CONTROL: {to} live again did not route"
        );
    }
}

/// **A RETIRE OF A ROW WRITTEN OVER SINCE IT WAS READ IS MOOT**: the retire names the
/// newest record the sweep read; a successor's row written since stays in the index and
/// the budget is unchanged.
#[cfg(unix)]
#[test]
fn a_retire_of_a_row_written_over_since_it_was_read_is_moot() {
    let (lane, mut bridge, mut push, mut reader, _server) = attached(&[(1, A), (2, B)]);
    mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    let face = subject::session_face("f", &bridge.node, B, "presence");
    let (read, _) = reader.last(&face, "", 1).unwrap();
    let off = read[0].0;
    reader
        .publish(
            bridge.producer_id,
            astream_broker::ACK_SEQ_BASE | (1 << 40),
            &face,
            b"v=1 t=9 inc=9 state=live",
        )
        .unwrap();
    let before = spent(&bridge, &mut reader);
    let log = bridge
        .conn
        .as_ref()
        .and_then(|c| c.seen_log_identity())
        .unwrap();
    assert_eq!(bridge.retire_held(&face, off, log), Some(false), "not moot");
    let row = indexed(&mut reader, &face);
    assert!(
        row.is_some_and(|row| row.contains("state=live")),
        "the successor's row went"
    );
    assert_eq!(spent(&bridge, &mut reader), before);
}

/// **A MOOT LANE RETIRE HOLDS THE ROW BACK**: B has left and its wait has passed, and a
/// record lands on the lane to B between the sweep's read and the lane's retire. The
/// lane's retire is moot, so B's row is not given back in that pass, nor B's pin dropped;
/// the lane's clock starts again with the record, and once a whole wait has passed both
/// go, and the pin with them.
#[cfg(unix)]
#[test]
fn a_moot_lane_retire_holds_the_row_and_the_pin_back() {
    let (lane, mut bridge, mut push, mut reader, _server) = attached(&[(1, A), (2, B)]);
    let (_, to_b, before) = mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    let node = bridge.node.clone();
    let b_row = subject::session_face("f", &node, B, "presence");
    bridge.state.pin(B, &node).unwrap();
    bridge.retire_ghost_presence();
    wait_passed(&mut bridge);
    bridge.written_before_retire = Some((to_b.clone(), b"v=1 t=9 kind=note text=late".to_vec()));
    bridge.retire_ghost_presence();
    assert!(
        bridge.written_before_retire.is_none(),
        "no retire of the lane"
    );
    assert_eq!(spent(&bridge, &mut reader), before, "a subject went");
    assert!(indexed(&mut reader, &to_b).is_some_and(|row| row.contains("text=late")));
    assert!(
        indexed(&mut reader, &b_row).is_some_and(|row| row.contains("state=exited")),
        "B's row went in the pass whose lane retire was moot"
    );
    assert!(bridge.state.pins().contains_key(B), "B's pin went");

    bridge.retire_ghost_presence();
    assert_eq!(
        spent(&bridge, &mut reader),
        before,
        "the lane's new record did not start its wait again"
    );
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(spent(&bridge, &mut reader), before - 2);
    assert!(!bridge.state.pins().contains_key(B), "B's pin stayed");
}

/// **A SESSION REVIVED BETWEEN THE READ AND THE RETIRE KEEPS ITS ROW AND ITS PIN**: B has
/// left and its wait has passed, and a successor (an update's other bridge, which this
/// one does not host it under) writes B's `live` row between the sweep's read and the
/// row's retire. The lane goes, the row's retire is moot, B's `live` row stays in the
/// index, B's pin stays, and B routes; the next sweep finds B live and gives nothing back.
#[cfg(unix)]
#[test]
fn a_session_revived_between_the_read_and_the_retire_keeps_its_row_and_its_pin() {
    let (lane, mut bridge, mut push, mut reader, _server) = attached(&[(1, A), (2, B)]);
    let (_, to_b, before) = mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    let node = bridge.node.clone();
    let b_row = subject::session_face("f", &node, B, "presence");
    bridge.state.pin(B, &node).unwrap();
    bridge.retire_ghost_presence();
    wait_passed(&mut bridge);
    bridge.written_before_retire = Some((b_row.clone(), b"v=1 t=9 inc=9 state=live".to_vec()));
    bridge.retire_ghost_presence();
    assert!(
        bridge.written_before_retire.is_none(),
        "no retire of B's row"
    );
    assert_eq!(indexed(&mut reader, &to_b), None, "the lane to B stayed");
    assert_eq!(spent(&bridge, &mut reader), before - 1);
    assert!(
        indexed(&mut reader, &b_row).is_some_and(|row| row.contains("state=live")),
        "the successor's row went"
    );
    assert_eq!(
        bridge.state.pins().get(B).map(String::as_str),
        Some(node.as_str()),
        "a revived session lost its pin"
    );
    assert_eq!(
        bridge.resolve_to("@s-b", Some(A)).unwrap(),
        Route::To(format!("/f/f/in/{node}/{B}/{node}"))
    );

    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(spent(&bridge, &mut reader), before - 1);
    assert!(bridge.state.pins().contains_key(B), "B's pin went");
}

/// **A PIN A KILLED BRIDGE LEFT BEHIND ITS RETIRES GOES AT THE NEXT ATTACH**: the reclaim
/// gave back everything this node held of B (here) and of `s-x` (on another node, whose
/// own reclaim gave its row back), and the bridge died before it dropped their pins. The
/// next bridge tracks nothing; its attach's sweep reads the pinned nodes, finds nothing of
/// either held, and drops both pins.
#[cfg(unix)]
#[test]
fn a_pin_a_killed_bridge_left_behind_its_retires_goes_at_the_next_attach() {
    let (lane, mut bridge, mut push, mut reader, _server) = attached(&[(1, A), (2, B)]);
    let (_, to_b, _) = mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    let (mut peer, x_row) = far_node_with_s_x(&bridge);
    let to_x = a_posts_to(&lane, &mut bridge, &mut reader, "@s-x");
    let node = bridge.node.clone();
    bridge.state.pin(B, &node).unwrap();
    peer.publish(FAR_PRODUCER, 3, &x_row, b"v=1 t=2 inc=1 state=exited")
        .unwrap();
    peer.retire_subject(FAR_PRODUCER, &x_row).unwrap();
    let log = bridge
        .conn
        .as_ref()
        .and_then(|c| c.seen_log_identity())
        .unwrap();
    let b_row = subject::session_face("f", &node, B, "presence");
    for subject in [&to_b, &b_row, &to_x] {
        let (rows, _) = reader.last(subject, "", 1).unwrap();
        assert_eq!(
            bridge.retire_held(subject, rows[0].0, log),
            Some(true),
            "{subject}"
        );
    }
    bridge.reclaim.clear();
    bridge.reclaim_owed = false;
    bridge.reclaim_remote = false;
    assert!(bridge.state.pins().contains_key(B) && bridge.state.pins().contains_key("s-x"));

    assert!(bridge.attach_broker(), "the attach");
    let pins = bridge.state.pins();
    assert!(!pins.contains_key(B), "B's pin stayed: {pins:?}");
    assert!(!pins.contains_key("s-x"), "s-x's pin stayed: {pins:?}");
}

/// **ONLY A REFUSAL FOR GOOD SWITCHES THE RECLAIM OFF, AND ONLY UNTIL THE NEXT ATTACH**: a
/// refusal of the read of the lanes this node writes that may not stand (`ERR_REFUSED`, a
/// failed read on the broker's side) gives nothing back that sweep and latches nothing:
/// the next sweep reads again and gives B's subjects back. One that stands
/// (`ERR_UNAUTHORIZED`) latches the reclaim off for the connection; the next attach asks
/// again.
#[cfg(unix)]
#[test]
fn only_a_refusal_for_good_switches_the_reclaim_off_until_the_next_attach() {
    let (lane, mut bridge, mut push, mut reader, _server) = attached(&[(1, A), (2, B)]);
    let (_, _, before) = mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    bridge.reclaim_read_refused = Some(astream_broker::proto::ERR_REFUSED);
    bridge.retire_ghost_presence();
    assert!(
        !bridge.reclaim_unread,
        "a refusal that may not stand latched"
    );
    assert!(
        bridge.reclaim_owed,
        "the refused sweep's look is not owed again"
    );
    assert!(bridge.sweep_owed());
    bridge.retire_ghost_presence();
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(spent(&bridge, &mut reader), before - 2);

    bridge.reclaim_read_refused = Some(astream_broker::proto::ERR_UNAUTHORIZED);
    bridge.reclaim_owed = true;
    bridge.retire_ghost_presence();
    assert!(bridge.reclaim_unread, "a refusal for good did not latch");
    assert!(!bridge.reclaim_looks());
    assert!(bridge.attach_broker(), "the attach");
    assert!(!bridge.reclaim_unread, "the attach did not ask again");
}

/// **AN ADDRESS NAMING THIS NODE AND A SID IT NEVER HAD IS UNROUTABLE**: no row, not
/// hosted; it spends no subject on a lane nobody reads.
#[cfg(unix)]
#[test]
fn an_address_naming_this_node_and_a_sid_with_no_row_is_unroutable() {
    let (_lane, mut bridge, _push, _reader, _server) = attached(&[(1, A)]);
    let node = bridge.node.clone();
    assert_eq!(
        bridge
            .resolve_to(&format!("@s-never@{node}"), Some(A))
            .unwrap(),
        Route::Unroutable
    );
    assert_eq!(
        bridge.resolve_to(&format!("@{A}@{node}"), Some(A)).unwrap(),
        Route::To(format!("/f/f/in/{node}/{A}/{node}"))
    );
}

/// **WHAT AN EARLIER RUN'S DEPARTED SESSION HOLDS IS FOUND AT THE NEXT ATTACH**: a bridge
/// that never saw B leave (its tracking empty, nothing owed) attaches, its attach's sweep
/// finds B's `exited` row and the lane to B, and once the wait has passed both go back.
#[cfg(unix)]
#[test]
fn what_an_earlier_runs_departed_session_holds_is_found_at_the_next_attach() {
    let (lane, mut bridge, mut push, mut reader, _server) = attached(&[(1, A), (2, B)]);
    let (_, to_b, _) = mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    bridge.reclaim.clear();
    bridge.reclaim_owed = false;
    bridge.retire_ghost_presence();
    assert!(bridge.reclaim.is_empty(), "a sweep owing nothing looked");

    assert!(bridge.attach_broker(), "the attach");
    assert!(bridge.reclaim.contains_key(&to_b), "{:?}", bridge.reclaim);
    let attached = spent(&bridge, &mut reader);
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(spent(&bridge, &mut reader), attached - 2);
}

/// **A LOG RETENTION CUT WITHOUT ITS INDEX REFUSES THE RECLAIM, SAID ONCE**: a log
/// compacted by astream's plain `retain_before` (not the state-carrying cut aterm's rules
/// run) refuses every retire `cut: `; the first refusal latches the log and nothing is
/// given back, then or by a later sweep.
#[cfg(unix)]
#[test]
fn a_log_cut_without_its_index_refuses_the_reclaim_once() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, A), (2, B)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = astream_broker::Broker::open(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    let mut push = FakePushLane::attach(&bridge);
    bridge.refresh_sessions().unwrap();
    bridge.admit_fresh_sessions();
    let (mut reader, _) = bridge.connect().unwrap();
    assert!(bridge.read_log_identity().unwrap().is_some());
    let (_, to_b, before) = mailed_then_b_left(&lane, &mut bridge, &mut push, &mut reader);
    broker.flush_durable().unwrap();
    // DELIBERATELY UNCARRIED: a plain cut on a broker no rules retain is the one astream does
    // not carry the state of (`BrokerLog::compact_begin` carries every cut on a retained log),
    // the cut this test is about.
    broker.retain_before(1).unwrap();

    bridge.retire_ghost_presence();
    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    let log = bridge.conn.as_ref().and_then(|c| c.seen_log_identity());
    assert_eq!(bridge.reclaim_cut, log, "the cut was not latched");
    assert_eq!(spent(&bridge, &mut reader), before);
    assert!(indexed(&mut reader, &to_b).is_some());

    wait_passed(&mut bridge);
    bridge.retire_ghost_presence();
    assert_eq!(bridge.reclaim_cut, log);
    assert_eq!(spent(&bridge, &mut reader), before);
}

/// **A RING THAT CANNOT READ THE LANES ITS NODE WRITES KEEPS THE LINK AND GIVES NOTHING
/// BACK**: on a guarded broker whose ring grants this node only its `ask` lanes, the
/// reclaim's read of every lane it writes is refused for good (`ERR_UNAUTHORIZED`). A
/// refusal is an answer: the link stays up, the reclaim is latched off until the next
/// attach (said once), nothing is retired, and the sweep is owed nothing more for it.
#[cfg(unix)]
#[test]
fn a_ring_that_cannot_read_its_lanes_keeps_the_link_and_gives_nothing_back() {
    let secret = [7u8; 32];
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, A), (2, B)]);
    let socket = lane.dir.path().join("broker.sock");
    let broker =
        astream_broker::Broker::open_guarded(lane.dir.path().join("broker.log"), secret.to_vec())
            .unwrap();
    let _server = broker.serve(&socket).unwrap();
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let node = bridge.node.clone();
    bridge.caps = [
        format!("rw,p={node}:/f/f/pub/{node}/>"),
        "ro:/f/f/pub/>".to_string(),
        format!("rw,p={node}:/f/f/in/*/*/{node}/ask"),
        format!("ro:/f/f/in/{node}/>"),
    ]
    .iter()
    .map(|grant| {
        let cap = astream_cap::mint(&secret, grant).unwrap();
        Cap {
            grant: cap.filter,
            tag: cap.tag,
        }
    })
    .collect();
    let (conn, _) = bridge.connect().unwrap();
    bridge.conn = Some(conn.into());
    let mut push = FakePushLane::attach(&bridge);
    bridge.refresh_sessions().unwrap();
    bridge.admit_fresh_sessions();
    assert!(bridge.conn.as_ref().is_some_and(|c| c.supports_retire()));
    let log = bridge.conn.as_ref().and_then(|c| c.seen_log_identity());
    assert!(log.is_some(), "the attach said Hello");
    lane.sessions.lock().unwrap().retain(|(_, sid)| sid != B);
    push.deliver(&mut bridge, &[&format!("EVENT 2 session-exited {B}")]);
    bridge.settle_acks();

    bridge.retire_ghost_presence();
    assert!(bridge.conn.is_some(), "a refused read dropped the link");
    assert!(bridge.reclaim_unread, "the refusal was not latched");
    assert!(
        !bridge.reclaim_looks(),
        "a latched reclaim still owes sweeps"
    );
    let (mut reader, _) = bridge.connect().unwrap();
    let row = indexed(
        &mut reader,
        &subject::session_face("f", &node, B, "presence"),
    );
    assert!(
        row.is_some_and(|row| row.contains("state=exited")),
        "B's row went"
    );
    bridge.retire_ghost_presence();
    assert!(bridge.conn.is_some());
}
