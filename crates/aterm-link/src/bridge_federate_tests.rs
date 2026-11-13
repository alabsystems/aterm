// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The bridge on the mesh (§11 C3): Macs each running a bridge on their own guarded `bus.log`, as
// the node service hosts it, under their node's ring, with each Mac's mail pushes
// (`crate::federate::Routes`) between them. Every wait is the one-minute hang detector.

use super::*;
use crate::federate::tests::{key_of, members, pid, routes_of, until, Mac, Net, Peer};
use crate::federate::{
    node_of, Handoff, Lane, MeshPaths, Routes, FLEET, HANDOFF_FILE, NODE_KEY_FILE,
};

/// A folder holding the node key of Mac `tag` and, where given, the node service's hand-off.
fn keys_for(tag: u8, handoff: Option<&Handoff>) -> aterm_tempfile::TempDir {
    let dir = crate::test_scratch_dir("keys");
    aterm_uds::private_file::ensure_private_dir(dir.path()).expect("a private dir");
    let seed = format!("{}\n", aterm_codec::hex::encode_lower(&[tag; 32]));
    aterm_uds::private_file::write_private(dir.path(), NODE_KEY_FILE, seed.as_bytes())
        .expect("the node key");
    if let Some(handoff) = handoff {
        handoff.write(dir.path()).expect("the hand-off");
    }
    dir
}

/// A bridge for `mac` with the mesh on, its node key and hand-off in `keys`, hosting `sid`, on a
/// fresh state dir or the one at `state`, its ring minted for its node: not yet attached.
fn bridge_on(
    mac: &Mac,
    sid: &str,
    keys: &std::path::Path,
    state: Option<&std::path::Path>,
    fx: crate::fx::Fx,
) -> (FakeVerbLane, Bridge, astream_broker::BrokerHandle) {
    bridge_hosting(mac, &[(1, sid)], keys, state, fx)
}

/// [`bridge_on`], hosting each of `sessions` (local id, sid).
fn bridge_hosting(
    mac: &Mac,
    sessions: &[(u64, &str)],
    keys: &std::path::Path,
    state: Option<&std::path::Path>,
    fx: crate::fx::Fx,
) -> (FakeVerbLane, Bridge, astream_broker::BrokerHandle) {
    let mesh = MeshPaths {
        keys: keys.to_path_buf(),
        root: keys.to_path_buf(),
    };
    let (lane, mut bridge) = FakeVerbLane::bridge_configured(sessions, state, Some(mesh));
    assert_eq!(
        (bridge.node.as_str(), bridge.cfg.fleet.as_str()),
        (mac.node.as_str(), FLEET),
        "the node is its key's, on the one fleet"
    );
    bridge.fx = fx;
    let socket = lane.dir.path().join("bus.sock");
    let server = mac.broker.serve(&socket).expect("bus.log served");
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    bridge.caps = crate::enable::node_rules(FLEET, &mac.node)
        .into_iter()
        .map(|grant| {
            let cap = astream_cap::mint(&mac.secret, &grant).expect("a grant");
            Cap {
                grant: cap.filter,
                tag: cap.tag,
            }
        })
        .collect();
    assert!(bridge.refresh_sessions().is_ok());
    (lane, bridge, server)
}

/// [`bridge_on`], attached.
fn attached(
    mac: &Mac,
    sid: &str,
    keys: &std::path::Path,
) -> (FakeVerbLane, Bridge, astream_broker::BrokerHandle) {
    let (lane, mut bridge, server) = bridge_on(mac, sid, keys, None, crate::fx::Fx::os());
    assert!(bridge.attach_broker(), "the bridge attaches to its bus.log");
    (lane, bridge, server)
}

/// `mac`'s pushes to `peers`, routed by `handoff`.
fn pushes(mac: &Mac, peers: &[&Mac], net: &Arc<Net>, handoff: &Handoff) -> Routes {
    let routes = routes_of(
        mac,
        peers.iter().map(|peer| Peer::of(peer)).collect(),
        net,
        false,
    );
    routes.set(handoff, &BTreeSet::new());
    routes
}

/// The `ev` records on `face` of `bridge`'s log, decoded.
fn evs_on(bridge: &Bridge, face: &str) -> Vec<String> {
    let (mut reader, _) = bridge.connect().expect("a reader");
    let (rows, _) = reader.fetch(0, face, 256).expect("the face");
    rows.iter()
        .filter_map(|(_, _, raw)| {
            let ev = Body::decode(raw).0.unknown.get("ev")?.clone();
            Some(crate::pct::decode(&ev))
        })
        .collect()
}

/// A post from `sid` on `lane`'s bridge to `to`.
fn queue(lane: &FakeVerbLane, sid: &str, id: u64, to: &str, kind: &str, text: &str) {
    *lane.outbox.lock().unwrap() = Some(format!(
        "post sid={sid} id={id} to={to} kind={kind} len={}\n{text}",
        text.len()
    ));
}

/// **A POST TO ANOTHER MAC REACHES ITS RING, AND THE RECEIPT NAMES THE ASKER'S RECORD**: a
/// session on A asks `@s-b@<B>`; A's bridge writes the ask on A's own `bus.log`, and A's push
/// carries it one hop to B's, under A's producer, numbered by its offset on A. B's bridge's
/// receipt for it, written on B's log, comes back the same way and names the ask by its offset on
/// A, where the asker waits on it, though B's log numbers it otherwise.
/// NEGATIVE CONTROL: B's bridge with the mesh off names the ask by its offset on B's own log,
/// which on A's names another record.
#[test]
fn a_post_to_another_mac_reaches_its_ring_and_the_receipt_names_the_askers_record() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (ma, mb) = (Mac::new(41), Mac::new(42));
        let nodes = [ma.node.as_str(), mb.node.as_str()];
        let (ka, kb) = (
            keys_for(41, Some(&members(&ma, &nodes))),
            keys_for(42, Some(&members(&mb, &nodes))),
        );
        let (lane_a, mut a, _served_a) = attached(&ma, "s-a", ka.path());
        let (_lane_b, mut b, _served_b) = attached(&mb, "s-b", kb.path());
        // B's log runs ahead of A's, so an offset on one names another record on the other.
        for _ in 0..7 {
            b.try_publish_ev_for(None, "filler").expect("filler");
        }
        let net = Arc::new(Net::default());
        let to_b = pushes(&ma, &[&mb], &net, &members(&ma, &nodes));
        let to_a = pushes(&mb, &[&ma], &net, &members(&mb, &nodes));

        queue(
            &lane_a,
            "s-a",
            1,
            &format!("@s-b@{}", mb.node),
            "ask",
            "hello",
        );
        a.drain_outbox();
        let ask = format!("/f/{FLEET}/in/{}/s-b/{}/ask", mb.node, ma.node);
        let sent = ma.holds(&ask);
        assert_eq!(
            sent.len(),
            1,
            "{case}: A's bridge wrote the ask on A's log: {:?}",
            lane_a.sent.lock().unwrap()
        );
        let at_a = sent[0].0;
        assert_eq!(
            sent[0].3.map(|(producer, _)| producer),
            Some(pid(&ma.node)),
            "{case}: under A's producer"
        );
        until(&format!("{case}: the ask reaches B's log"), || {
            !mb.holds(&ask).is_empty()
        });
        let got = mb.holds(&ask);
        assert_eq!(got.len(), 1, "{case}: once");
        let at_b = got[0].0;
        assert_eq!(
            got[0].3,
            Some((pid(&ma.node), at_a)),
            "{case}: numbered by its offset on A"
        );
        assert_ne!(at_b, at_a, "{case}: B's log numbers it otherwise");
        assert_eq!(Body::decode(&got[0].2).0.text, "hello", "{case}");

        if control {
            b.cfg.mesh = None;
        }
        b.send_receipt(&OwedReceipt {
            sid: "s-b".to_owned(),
            rid: 1,
            off: at_b,
            verdict: "handled".to_owned(),
            kind: "ask".to_owned(),
            from: format!("s-a@{}", ma.node),
            earlier_log: false,
        })
        .expect("the receipt goes");
        let ack = format!("/f/{FLEET}/in/{}/s-a/{}/ack", ma.node, mb.node);
        until(&format!("{case}: the receipt reaches A's log"), || {
            !ma.holds(&ack).is_empty()
        });
        let back = ma.holds(&ack);
        assert_eq!(
            back[0].3.map(|(producer, _)| producer),
            Some(pid(&mb.node)),
            "{case}: under B's producer"
        );
        let receipt = Body::decode(&back[0].2).0;
        assert_eq!(receipt.verdict.as_deref(), Some("handled"), "{case}");
        assert_eq!(
            receipt.re,
            Some(if control { at_b } else { at_a }),
            "{case}: the receipt names the ask"
        );
        drop((to_a, to_b));
    }
}

/// **ROUTING TO ANOTHER MAC FAILS CLOSED ON THE HAND-OFF**: while the node service's hand-off is
/// missing, stale (it names another node as this Mac's, as one written before this Mac's key
/// changed does) or unreadable, a post to another Mac's session is retired `unroutable` with
/// nothing written for it and the bus says why (`why=hand-off-…` on the node's `undeliverable`),
/// and a receipt owed there is recorded unsent with the same word; this Mac's own sessions are
/// routed to all the while.
/// NEGATIVE CONTROL: with a current hand-off the same post is written to the lane the push
/// carries.
#[test]
fn routing_to_another_mac_fails_closed_without_a_current_hand_off() {
    let ma = Mac::new(43);
    let far = node_of(&key_of(44));
    let nodes = [ma.node.as_str(), far.as_str()];
    let keys = keys_for(43, None);
    let (lane, mut a, _served) = attached(&ma, "s-a", keys.path());
    let current = members(&ma, &nodes);
    let stale = Handoff {
        me: node_of(&key_of(45)),
        ..current.clone()
    };
    let to = format!("@s-b@{far}");
    let lane_far = format!("/f/{FLEET}/in/{far}/s-b/{}/note", ma.node);
    let own = Route::To(format!("/f/{FLEET}/in/{}/s-a/{}", ma.node, ma.node));
    let session_ev = subject::session_face(FLEET, &ma.node, "s-a", "ev");
    let cases: [(&str, &dyn Fn()); 4] = [
        ("hand-off-missing", &|| {}),
        ("hand-off-stale", &|| {
            stale.write(keys.path()).expect("stale")
        }),
        ("hand-off-unreadable", &|| {
            let torn = format!("{}member n-xyz\n", current.render());
            aterm_uds::private_file::write_private(keys.path(), HANDOFF_FILE, torn.as_bytes())
                .expect("torn");
        }),
        ("", &|| current.write(keys.path()).expect("current")),
    ];
    for (id, (why, set)) in (1u64..).zip(cases) {
        set();
        queue(&lane, "s-a", id, &to, "note", "hi");
        a.drain_outbox();
        let sent = lane.sent.lock().unwrap().clone();
        if why.is_empty() {
            assert_eq!(ma.holds(&lane_far).len(), 1, "the control routes: {sent:?}");
        } else {
            assert!(ma.holds(&lane_far).is_empty(), "{why}: nothing written");
            assert!(
                sent.contains(&format!("outbox sent s-a {id} off=- reason=unroutable")),
                "{why}: the post is retired: {sent:?}"
            );
            let said = format!("undeliverable to={to} reason=unroutable why={why}");
            let node_ev = subject::node_face(FLEET, &ma.node, "ev");
            assert!(
                evs_on(&a, &node_ev).contains(&said),
                "{why}: the bus says why: {:?}",
                evs_on(&a, &node_ev)
            );
            a.send_receipt(&OwedReceipt {
                sid: "s-a".to_owned(),
                rid: id,
                off: 1,
                verdict: "handled".to_owned(),
                kind: "ask".to_owned(),
                from: format!("s-b@{far}"),
                earlier_log: false,
            })
            .expect("recorded unsent");
            let unacked = format!("unacked re=1 verdict=handled reason={why}");
            assert!(
                evs_on(&a, &session_ev).contains(&unacked),
                "{why}: {:?}",
                evs_on(&a, &session_ev)
            );
        }
        assert_eq!(
            a.resolve_to("@s-a", None).expect("a route"),
            own,
            "{why}: this Mac's own sessions are routed to"
        );
    }
}

/// Every record on `mac`'s log under `mac`'s producer that `bridge`'s incarnation `inc` did not
/// number: an incarnation's sequence it reserved (up to `reserved`), or its will.
fn not_the_bridges(mac: &Mac, inc: u64, reserved: u64) -> Vec<(u64, String, u64)> {
    mac.holds(&format!("/f/{FLEET}/>"))
        .into_iter()
        .filter_map(|(off, subject, _, origin)| {
            let (producer, seq) = origin.expect("every record stamped");
            let numbered = astream_broker::incarnation_of(seq) == Some(inc)
                && (seq <= reserved || astream_broker::will_seq(inc) == Some(seq));
            (producer == pid(&mac.node) && !numbered).then_some((off, subject, seq))
        })
        .collect()
}

/// The highest sequence under `mac`'s producer on its own log.
fn last_seq(mac: &Mac) -> u64 {
    mac.holds(&format!("/f/{FLEET}/>"))
        .into_iter()
        .filter_map(|(_, _, _, origin)| origin.filter(|(producer, _)| *producer == pid(&mac.node)))
        .map(|(_, seq)| seq)
        .max()
        .expect("the bridge wrote")
}

/// **THE BRIDGE IS ITS LOG'S ONE WRITER UNDER ITS NODE'S PRODUCER**: after mail both ways,
/// every record on each Mac's `bus.log` under that Mac's producer is one its bridge numbered (its
/// incarnation, a sequence it reserved, or its will); the other Mac's mail there is under the
/// other's producer, numbered by offsets the bridge's numbering never meets.
/// NEGATIVE CONTROL: a push whose destination leads back to its own log (a member whose address
/// is this Mac) is refused by astream (no federation onto the same log), and a record that push
/// would have written, under A's producer numbered by an offset, stands in for it: the check
/// names it.
#[test]
fn the_bridge_is_its_logs_one_writer_under_its_nodes_producer() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (ma, mb) = (Mac::new(46), Mac::new(47));
        let looped = node_of(&key_of(48));
        let nodes = [ma.node.as_str(), mb.node.as_str(), looped.as_str()];
        let (ka, kb) = (
            keys_for(46, Some(&members(&ma, &nodes))),
            keys_for(47, Some(&members(&mb, &nodes))),
        );
        let (lane_a, mut a, _served_a) = attached(&ma, "s-a", ka.path());
        let (lane_b, mut b, _served_b) = attached(&mb, "s-b", kb.path());
        let net = Arc::new(Net::default());
        let mut peers_of_a = vec![Peer::of(&mb)];
        if control {
            peers_of_a.push(Peer {
                node: looped.clone(),
                ..Peer::of(&ma)
            });
        }
        let to_b = routes_of(&ma, peers_of_a, &net, false);
        to_b.set(&members(&ma, &nodes), &BTreeSet::new());
        let to_a = pushes(&mb, &[&ma], &net, &members(&mb, &nodes));

        queue(
            &lane_a,
            "s-a",
            1,
            &format!("@s-b@{}", mb.node),
            "note",
            "a to b",
        );
        a.drain_outbox();
        queue(
            &lane_b,
            "s-b",
            1,
            &format!("@s-a@{}", ma.node),
            "note",
            "b to a",
        );
        b.drain_outbox();
        queue(
            &lane_a,
            "s-a",
            2,
            &format!("@s-x@{looped}"),
            "note",
            "a to x",
        );
        a.drain_outbox();
        let (ab, ba) = (
            format!("/f/{FLEET}/in/{}/s-b/{}/note", mb.node, ma.node),
            format!("/f/{FLEET}/in/{}/s-a/{}/note", ma.node, mb.node),
        );
        until(&format!("{case}: mail both ways"), || {
            !mb.holds(&ab).is_empty() && !ma.holds(&ba).is_empty()
        });
        let loop_lane = format!("/f/{FLEET}/in/{looped}/s-x/{}/note", ma.node);
        if control {
            // astream refuses the push onto its own log outright ...
            let looping = Lane::Mail {
                dst: looped.clone(),
            };
            until(&format!("{case}: the loop is refused"), || {
                to_b.status().get(&looping).is_some_and(|status| {
                    status
                        .last_error
                        .as_deref()
                        .is_some_and(|e| e.contains("self-federation"))
                })
            });
            assert_eq!(ma.holds(&loop_lane).len(), 1, "{case}: nothing copied");
            // ... so the second writer stands in: what that push would write, by offset.
            ma.post(&looped, 1, b"a copy");
        }
        for (mac, bridge, other) in [(&ma, &a, &mb), (&mb, &b, &ma)] {
            let foreign = not_the_bridges(mac, bridge.inc, bridge.state.sequence());
            if control && mac.node == ma.node {
                assert_eq!(
                    foreign
                        .iter()
                        .map(|(_, subject, seq)| (subject.as_str(), *seq))
                        .collect::<Vec<_>>(),
                    [(crate::federate::tests::lane(&looped, &ma.node).as_str(), 1)],
                    "{case}: the second writer under A's producer is named"
                );
                continue;
            }
            assert_eq!(foreign, [], "{case}: on {}'s log", mac.node);
            let from_other = mac
                .holds(&format!("/f/{FLEET}/in/{}/>", mac.node))
                .into_iter()
                .filter(|(_, _, _, origin)| origin.is_some_and(|(p, _)| p == pid(&other.node)))
                .count();
            assert_eq!(
                from_other, 1,
                "{case}: the other Mac's note, under its producer"
            );
        }
        drop((to_a, to_b));
    }
}

/// **A RESTART NUMBERS ABOVE THE LAST**: A's bridge restarts on a state dir that lost everything,
/// its node kept by its node key (§11 C3): it takes an incarnation above the one its will names on
/// the bus, its first sequence is above every one before it under A's producer, and its next post
/// lands on A's log and reaches B's.
/// NEGATIVE CONTROL: [`crate::sim::world::Mutant::IgnoreBusIncarnation`]: the restarted bridge
/// takes its incarnation without the bus's, and numbers from the start again, below the last.
#[test]
fn a_bridge_restarted_without_its_state_numbers_above_the_last() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (ma, mb) = (Mac::new(49), Mac::new(50));
        let nodes = [ma.node.as_str(), mb.node.as_str()];
        let ka = keys_for(49, Some(&members(&ma, &nodes)));
        let net = Arc::new(Net::default());
        let to_b = pushes(&ma, &[&mb], &net, &members(&ma, &nodes));
        let (lane, mut first, served) = attached(&ma, "s-a", ka.path());
        queue(
            &lane,
            "s-a",
            1,
            &format!("@s-b@{}", mb.node),
            "note",
            "before",
        );
        first.drain_outbox();
        let was = first.inc;
        first.close_subscriptions();
        // The bridge goes; the log stays served, as the node service keeps it.
        drop((first, lane));
        let ab = format!("/f/{FLEET}/in/{}/s-b/{}/note", mb.node, ma.node);
        until(&format!("{case}: the first note reaches B"), || {
            mb.holds(&ab).len() == 1
        });
        let last = last_seq(&ma);

        let fx = if control {
            crate::fx::Fx::os_without(crate::sim::world::Mutant::IgnoreBusIncarnation)
        } else {
            crate::fx::Fx::os()
        };
        let (lane, mut again, _served_again) = bridge_on(&ma, "s-a", ka.path(), None, fx);
        let attached = again.attach_broker();
        if control {
            let first_seq = again.next_seq().expect("a sequence");
            assert!(
                first_seq <= last,
                "{case}: the start again ({first_seq}) is below the last ({last}), attached={attached}"
            );
            continue;
        }
        assert!(attached, "{case}: the restart attaches");
        assert!(
            again.inc > was,
            "{case}: incarnation {} after {was}",
            again.inc
        );
        queue(
            &lane,
            "s-a",
            1,
            &format!("@s-b@{}", mb.node),
            "note",
            "after",
        );
        again.drain_outbox();
        let after: Vec<u64> = ma
            .holds(&format!("/f/{FLEET}/>"))
            .into_iter()
            .filter_map(|(_, _, _, origin)| origin.filter(|(p, _)| *p == pid(&ma.node)))
            .map(|(_, seq)| seq)
            .filter(|seq| astream_broker::incarnation_of(*seq) == Some(again.inc))
            .collect();
        assert!(
            after.iter().all(|seq| *seq > last) && !after.is_empty(),
            "{case}: {after:?} above {last}"
        );
        until(&format!("{case}: the next note reaches B"), || {
            mb.holds(&ab).len() == 2
        });
        drop((to_b, served));
    }
}

/// **THE BRIDGE NAMES THE TOPICS ITS SESSIONS HOLD**: with the mesh on, A's bridge writes the set
/// of topics its sessions hold on its node's topics face when an attach starts and whenever the
/// set changes, and only then, so the node service names them in A's presence row and the other
/// Macs push their broadcasts here while it names one.
/// NEGATIVE CONTROL: a bridge with the mesh off holds the same topics and writes nothing there.
#[test]
fn the_bridge_names_the_topics_its_sessions_hold_on_its_nodes_face() {
    use crate::federate::{topics_face, topics_of, TOPICS_LEAF};
    let set = |topics: &[&str]| -> BTreeSet<String> {
        topics.iter().map(|topic| (*topic).to_owned()).collect()
    };
    let ma = Mac::new(47);
    let keys = keys_for(47, None);
    let (_lane, mut a, _served) = attached(&ma, "s-a", keys.path());
    let named = || -> Vec<BTreeSet<String>> {
        ma.holds(&topics_face(&ma.node))
            .into_iter()
            .map(|(_, _, body, _)| topics_of(&body).expect("a topics body"))
            .collect()
    };
    assert_eq!(named(), [set(&[])], "the attach names none held");
    let hold = |bridge: &mut Bridge, words: &str| {
        bridge.topic_change("s-a", &mut words.split_whitespace());
        assert!(bridge.sync_say_face());
    };
    hold(&mut a, "add build since=@0");
    assert!(a.sync_say_face(), "a sync with no change");
    hold(&mut a, "add deploy since=@0");
    hold(&mut a, "add deploy since=@0");
    assert_eq!(
        named(),
        [set(&[]), set(&["build"]), set(&["build", "deploy"])],
        "one record per change, none for a sync that changed nothing"
    );

    let (_lane, mut off, _server) = rig_with_broker(&[(1, "s-a")]);
    assert!(off.cfg.mesh.is_none());
    off.refresh_sessions().expect("sessions");
    hold(&mut off, "add build since=@0");
    assert!(off.say_face_held(), "the control holds the topic");
    let face = subject::node_face(&off.cfg.fleet, &off.node, TOPICS_LEAF);
    let (mut reader, _) = off.connect().expect("a reader");
    let (rows, _) = reader.fetch(0, &face, 16).expect("the face");
    assert!(
        rows.is_empty(),
        "with the mesh off nothing is named: {rows:?}"
    );
}

/// Rounds of `bridge` until `done`.
fn rounds_until(what: &str, bridge: &mut Bridge, mut done: impl FnMut() -> bool) {
    until(what, || {
        bridge.round(Duration::ZERO);
        done()
    });
}

/// **UNREAD MAIL FOR A MAC'S OLD NODE REACHES ITS SESSION ONCE** (§11 C3b): B's ask to `s-a`
/// landed on A's `bus.log` for A's node after A's bridge had read and passed a note there, and
/// before A's node key rotated. A's node service, under the new node on the same log, moves what
/// the old node's group had not passed to the new node's lanes, under their subjects; A's bridge,
/// attached as the new node, delivers the ask to `s-a` once, beside a note to the new node, and
/// never the note its old bridge delivered; its receipt names the ask by its offset on B, where
/// the asker waits on it, read where the ask came from under the node service's reader's ring
/// ([`crate::federate::OLD_READS_FILE`]), though the bridge attached before the hand-off named
/// the old node. An ask A's own session wrote under the old node moves too, reaches `s-a` from
/// that session, and its receipt names it by its offset on A, where that asker waits, on the lane
/// of the node A holds now. A person's post on A to the old node moves the same
/// way, and A's person carry (§11 C3c) leaves it to the move, saying nothing of it. A restart of
/// the push moves nothing again: an ask B sends to the old node later, its hand-off not yet
/// turned, follows the first alone.
/// NEGATIVE CONTROL: [`crate::federate::ONE_NODE`], the pushes before C3b: no such push, so the
/// bridge's one inbox, the new node's, never reads the ask, and the old group holds it, while
/// the note to the new node reaches `s-a`; and the carry calls the old node no member. And
/// without the reader's ring the late ask's receipt names it by its offset on A's log.
#[test]
fn unread_mail_for_a_macs_old_node_reaches_its_session_once() {
    use crate::federate::{mail_group, tests::routes_as, ONE_NODE};
    for control in [false, true] {
        let case = format!("control={control}");
        let (ma, mb) = (Mac::new(53), Mac::new(54));
        let new = ma.rotated(63);
        let before = members(&ma, &[&ma.node, &mb.node]);
        let after = Handoff {
            me: new.node.clone(),
            members: [new.node.clone(), mb.node.clone()].into(),
            moved: [(ma.node.clone(), new.node.clone())].into(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        };
        // B's mail for `s-a` at `dst`, as B's push writes it on A's log: under B's producer,
        // numbered by its offset on B.
        let from_b = |dst: &str, kind: &str, on_b: u64, text: &str| -> u64 {
            let subject = format!("/f/{FLEET}/in/{dst}/s-a/{}/{kind}", mb.node);
            let mut push = ma.client(
                mb.key,
                &[format!(
                    "rw,p={}:/f/{FLEET}/in/{dst}/*/{}/*",
                    mb.node, mb.node
                )],
            );
            let mut body = Body::new(0);
            body.from = Some("s-b".to_owned());
            body.text = text.to_owned();
            push.publish(pid(&mb.node), on_b, &subject, &body.encode())
                .expect("B's push lands")
                .0
        };

        // A's bridge as the old node reads the first note and its group passes it.
        let k_old = keys_for(53, Some(&before));
        let (lane_old, mut old, served_old) = attached(&ma, "s-a", k_old.path());
        let read = from_b(&ma.node, "note", 3, "read before");
        rounds_until(
            &format!("{case}: the old node's bridge delivers the note"),
            &mut old,
            || {
                lane_old.delivery_offsets() == [read]
                    && ma.broker.group_start(&mail_group(&ma.node)) > read
            },
        );
        old.close_subscriptions();
        // The bridge goes; the log stays served, as the node service keeps it.
        drop((old, lane_old));
        let ask = from_b(&ma.node, "ask", 7, "unread");
        // And an ask of A's own session `s-x`, as A's bridge wrote it under the old node: under
        // that node's producer, numbered in the bridge's incarnation past what it wrote above.
        let local = {
            let mut own = ma.client(
                ma.key,
                &[format!("rw,p={0}:/f/{FLEET}/in/{0}/*/{0}/*", ma.node)],
            );
            let mut body = Body::new(0);
            body.from = Some("s-x".to_owned());
            body.text = "asked here".to_owned();
            let subject = format!("/f/{FLEET}/in/{0}/s-a/{0}/ask", ma.node);
            let (at, deduped) = own
                .publish(pid(&ma.node), (1 << 32) | 1000, &subject, &body.encode())
                .expect("A's own ask lands");
            assert!(!deduped, "{case}: a sequence the old bridge never wrote");
            at
        };

        // A's bridge, as the node A holds next, attaches under a hand-off that names no rotation
        // yet; `s-x` is a session of A's too, so its bridge vouches for no sender by that name,
        // and the ask reads as `s-x`'s only where the bridge knows the node it came under is A's.
        let unturned = Handoff {
            moved: BTreeMap::new(),
            ..after.clone()
        };
        let k_new = keys_for(63, Some(&unturned));
        let (lane, mut a, _served) = attached(&new, "s-a", k_new.path());
        lane.sessions.lock().unwrap().push((2, "s-x".to_owned()));
        a.refresh_sessions().expect("the sessions");

        // A's node key rotates: the node service's next generation, on the same bus.log, writes
        // its reader's ring and the hand-off that names the old node, then starts its pushes; the
        // bridge reads the hand-off at its next inbox run.
        crate::federate::write_old_reads(k_new.path(), &after, &|grant| {
            Mac::mint(&ma.secret, grant)
        })
        .expect("the node service's reader's ring");
        after.write(k_new.path()).expect("the hand-off");
        ONE_NODE.with(|on| on.set(control));
        let net = Arc::new(Net::default());
        let olds = BTreeSet::from([ma.node.clone()]);
        let start = || {
            let routes = routes_as(
                &ma,
                (&new.node, new.key),
                &olds,
                vec![Peer::of(&mb)],
                &net,
                false,
            );
            routes.set(&after, &BTreeSet::new());
            routes
        };
        let routes = start();
        let moved_lane = format!("/f/{FLEET}/in/{}/s-a/{}/ask", new.node, mb.node);
        let moved = || -> Vec<u64> {
            new.holds(&moved_lane)
                .into_iter()
                .map(|(off, _, _, _)| off)
                .collect()
        };
        let own_lane = format!("/f/{FLEET}/in/{}/s-a/{}/ask", new.node, ma.node);
        let moved_own = || -> Vec<u64> {
            new.holds(&own_lane)
                .into_iter()
                .map(|(off, _, _, _)| off)
                .collect()
        };
        if !control {
            until(&format!("{case}: the asks move to the new node"), || {
                moved().len() == 1 && moved_own().len() == 1
            });
            until(&format!("{case}: the old group passes them"), || {
                ma.broker.group_start(&mail_group(&ma.node)) > local
            });
        }
        // A person's post on A to a session of A's old node is A's own mail too: the move takes
        // it, and the person carry, which carries a person's posts to the other Macs, leaves it
        // be, where under ONE_NODE it takes the old node for no member and says so. Each post
        // goes in order, so the notice for a later post to no member says the first was taken.
        let person_lane = format!("/f/{FLEET}/in/{}/s-q/h-andrew/note", new.node);
        let post = |dst: &str, seq| {
            person_posts(
                &ma,
                k_old.path(),
                (dst, seq, "note", "a person's"),
                Seal::Bare,
            )
        };
        post(&ma.node, 1);
        let unmet = post(&node_of(&key_of(64)), 2);
        let notices = format!(
            "{}/undeliverable",
            subject::person_lane(FLEET, "h-andrew", &new.node)
        );
        // The carry counts a refusal after its notice is on the log, so the wait reads both.
        let refused = if control { 2 } else { 1 };
        until(
            &format!("{case}: the carry took the post to no member"),
            || {
                routes.carry_status().refused >= refused
                    && new
                        .holds(&notices)
                        .iter()
                        .any(|(_, _, body, _)| Body::decode(body).0.re == Some(unmet))
            },
        );
        let status = routes.carry_status();
        assert_eq!(
            (status.carried, status.refused),
            (0, refused),
            "{case}: {status:?}"
        );
        if !control {
            until(&format!("{case}: the person's post moves"), || {
                new.holds(&person_lane).len() == 1
            });
        }
        let fresh = from_b(&new.node, "note", 9, "to the new node");
        let mut want = moved();
        want.extend(moved_own());
        want.sort_unstable();
        want.push(fresh);
        rounds_until(&format!("{case}: the bridge delivers"), &mut a, || {
            lane.delivery_offsets().contains(&fresh)
        });
        assert_eq!(lane.delivery_offsets(), want, "{case}: each once, in order");
        assert!(
            !lane.delivery_offsets().contains(&read),
            "{case}: never the note the old bridge delivered"
        );
        if control {
            assert_eq!(moved(), [], "{case}: nothing moved");
            assert!(
                ma.broker.group_start(&mail_group(&ma.node)) <= ask,
                "{case}: the old group holds the ask, unread"
            );
            ONE_NODE.with(|on| on.set(false));
            drop((routes, served_old));
            continue;
        }
        assert!(
            lane.deliveries()[0].contains(&format!("from=s-b@{}", mb.node)),
            "{case}: from B's asker: {:?}",
            lane.deliveries()
        );
        a.send_receipt(&OwedReceipt {
            sid: "s-a".to_owned(),
            rid: 1,
            off: want[0],
            verdict: "handled".to_owned(),
            kind: "ask".to_owned(),
            from: format!("s-b@{}", mb.node),
            earlier_log: false,
        })
        .expect("the receipt goes");
        let ack = format!("/f/{FLEET}/in/{}/s-b/{}/ack", mb.node, new.node);
        let receipts = new.holds(&ack);
        assert_eq!(receipts.len(), 1, "{case}: the receipt is on A's log");
        assert_eq!(
            Body::decode(&receipts[0].2).0.re,
            Some(7),
            "{case}: the receipt names the ask by its offset on B"
        );
        // A's own asker waits on the offset its ask has here: its receipt names that one, not
        // the copy the move made, and goes to the asker on the node A holds now.
        let own = moved_own()[0];
        assert!(
            lane.deliveries()
                .iter()
                .any(|line| line.contains(&format!(" off={own} "))
                    && line.contains(&format!("from=s-x@{}", ma.node))),
            "{case}: from A's own asker: {:?}",
            lane.deliveries()
        );
        a.send_receipt(&OwedReceipt {
            sid: "s-a".to_owned(),
            rid: 3,
            off: own,
            verdict: "handled".to_owned(),
            kind: "ask".to_owned(),
            from: format!("s-x@{}", ma.node),
            earlier_log: false,
        })
        .expect("the receipt goes");
        let own_ack = format!("/f/{FLEET}/in/{0}/s-x/{0}/ack", new.node);
        assert_eq!(
            new.holds(&own_ack)
                .iter()
                .map(|(_, _, body, _)| Body::decode(body).0.re)
                .collect::<Vec<_>>(),
            [Some(local)],
            "{case}: the receipt names A's own ask by its offset here, not the copy's {own}"
        );

        // The push restarts; B, its hand-off not yet turned, asks the old node again.
        drop(routes);
        let routes = start();
        let late = from_b(&ma.node, "ask", 11, "late");
        until(&format!("{case}: the late ask moves"), || {
            moved().len() == 2
        });
        // The old group's commit is its own write, answered after the move lands on the new node,
        // so it is waited for as the first asks' is above, not read the moment the move shows.
        until(
            &format!("{case}: the old group passes the late ask"),
            || ma.broker.group_start(&mail_group(&ma.node)) > late,
        );
        let held = new.holds(&moved_lane);
        assert_eq!(
            held.iter()
                .map(|(_, _, body, _)| Body::decode(body).0.text)
                .collect::<Vec<_>>(),
            ["unread", "late"],
            "{case}: the restart moved the first ask no second time"
        );
        rounds_until(
            &format!("{case}: the late ask is delivered"),
            &mut a,
            || lane.delivery_offsets().contains(&held[1].0),
        );

        // NEGATIVE CONTROL of the receipt: without the node service's reader's ring the bridge
        // cannot read where the late ask came from, and names it by its offset here instead.
        std::fs::remove_file(k_new.path().join(crate::federate::OLD_READS_FILE))
            .expect("the ring goes");
        a.send_receipt(&OwedReceipt {
            sid: "s-a".to_owned(),
            rid: 2,
            off: held[1].0,
            verdict: "handled".to_owned(),
            kind: "ask".to_owned(),
            from: format!("s-b@{}", mb.node),
            earlier_log: false,
        })
        .expect("the receipt goes");
        let receipts = new.holds(&ack);
        assert_eq!(
            receipts
                .iter()
                .map(|(_, _, body, _)| Body::decode(body).0.re)
                .collect::<Vec<_>>(),
            [Some(7), Some(held[1].0)],
            "{case}: the late ask's receipt names it as A's log numbers it, not 11"
        );
        drop((routes, served_old));
    }
}

/// **UNREAD MAIL UNDER THE NODE A MAC RETIRED LAST REACHES ITS SESSION ONCE** (§11 C3b): A's seat
/// retired one more node than a ring names, the one it retired last the greatest in node order,
/// and B's asks to `s-a` wait unread on A's `bus.log` under that node and under the one A retired
/// first. A's node service moves the mail of the nodes its seat retired last
/// ([`Handoff::ringed`]) and writes the reader's ring that reads them; A's bridge follows that
/// ring, whichever nodes it reads, so it delivers the ask under the node retired last once, and
/// its receipt names the ask by its offset on B, read where the ask came from. The ask under the
/// node retired first stays on its old group, unread.
/// NEGATIVE CONTROL: [`crate::federate::NODE_ORDER_RING`], the choice before the second recency
/// phase: the moves and the reader's ring take the first nodes in node order, so the ask under
/// the node retired last never reaches `s-a` and its old group holds it, while the one under the
/// node retired first moves.
#[test]
fn unread_mail_under_the_node_a_mac_retired_last_reaches_its_session_once() {
    use crate::federate::{mail_group, tests::routes_as, NODE_ORDER_RING, RING_OLDS_MAX};
    for control in [false, true] {
        let case = format!("control={control}");
        let (ma, mb) = (Mac::new(55), Mac::new(56));
        let now = ma.rotated(65);
        let olds: BTreeSet<String> = (0..=RING_OLDS_MAX)
            .map(|i| node_of(&key_of(170 + u8::try_from(i).unwrap())))
            .collect();
        // Retired in node order: the greatest node last, which node order leaves out.
        let (first, last) = (
            olds.first().expect("nine").clone(),
            olds.last().expect("nine").clone(),
        );
        let after = Handoff {
            me: now.node.clone(),
            members: [now.node.clone(), mb.node.clone()].into(),
            moved: olds
                .iter()
                .map(|old| (old.clone(), now.node.clone()))
                .collect(),
            rank: olds.iter().rev().cloned().zip(0..).collect(),
            gone: BTreeSet::new(),
        };
        // B's ask to `s-a` at `dst`, as B's push writes it on A's log: under B's producer,
        // numbered by its offset on B.
        let from_b = |dst: &str, on_b: u64, text: &str| -> u64 {
            let subject = format!("/f/{FLEET}/in/{dst}/s-a/{}/ask", mb.node);
            let mut push = ma.client(
                mb.key,
                &[format!(
                    "rw,p={}:/f/{FLEET}/in/{dst}/*/{}/*",
                    mb.node, mb.node
                )],
            );
            let mut body = Body::new(0);
            body.from = Some("s-b".to_owned());
            body.text = text.to_owned();
            push.publish(pid(&mb.node), on_b, &subject, &body.encode())
                .expect("B's push lands")
                .0
        };
        let under_last = from_b(&last, 7, "under the last");
        let under_first = from_b(&first, 8, "under the first");
        let (moves, stays, stays_at, text) = if control {
            (&first, &last, under_last, "under the first")
        } else {
            (&last, &first, under_first, "under the last")
        };

        // The node service writes its reader's ring and the hand-off, then starts its pushes.
        let keys = keys_for(65, Some(&after));
        NODE_ORDER_RING.with(|on| on.set(control));
        crate::federate::write_old_reads(keys.path(), &after, &|grant| {
            Mac::mint(&ma.secret, grant)
        })
        .expect("the node service's reader's ring");
        let net = Arc::new(Net::default());
        let routes = routes_as(&ma, (&now.node, now.key), &olds, Vec::new(), &net, false);
        NODE_ORDER_RING.with(|on| on.set(false));
        routes.set(&after, &BTreeSet::new());
        let moved_lane = format!("/f/{FLEET}/in/{}/s-a/{}/ask", now.node, mb.node);
        until(&format!("{case}: the ask under {moves} moves"), || {
            now.holds(&moved_lane).len() == 1
                && ma.broker.group_start(&mail_group(moves))
                    > if control { under_first } else { under_last }
        });
        let held = now.holds(&moved_lane);
        assert_eq!(
            Body::decode(&held[0].2).0.text,
            text,
            "{case}: the ask under the node its ring names"
        );
        let copy = held[0].0;
        assert!(
            ma.broker.group_start(&mail_group(stays)) <= stays_at,
            "{case}: the ask under {stays} stays on its old group, unread"
        );

        let (lane, mut a, _served) = attached(&now, "s-a", keys.path());
        rounds_until(&format!("{case}: the bridge delivers"), &mut a, || {
            lane.delivery_offsets() == [copy]
        });
        if control {
            // The one copy is the ask under the node retired first: the one under the node
            // retired last never reaches s-a.
            drop(routes);
            continue;
        }
        a.send_receipt(&OwedReceipt {
            sid: "s-a".to_owned(),
            rid: 1,
            off: copy,
            verdict: "handled".to_owned(),
            kind: "ask".to_owned(),
            from: format!("s-b@{}", mb.node),
            earlier_log: false,
        })
        .expect("the receipt goes");
        let ack = format!("/f/{FLEET}/in/{}/s-b/{}/ack", mb.node, now.node);
        let receipts = now.holds(&ack);
        assert_eq!(receipts.len(), 1, "{case}: the receipt is on A's log");
        assert_eq!(
            Body::decode(&receipts[0].2).0.re,
            Some(7),
            "{case}: the receipt names the ask by its offset on B, read under the ring"
        );
        drop(routes);
    }
}

/// **MAIL UNREAD ACROSS TWO ROTATIONS OF A MAC'S KEY REACHES ITS SESSION ONCE** (§11 C3b): B's ask
/// to `s-a` lands on A's `bus.log` for A's node, and A's key rotates twice before `s-a` reads it.
/// The node service under the second new node moves the first move's copy on, and A's bridge
/// delivers the ask once, naming its first offset on A (`was=`) through both moves; its receipt
/// follows the copy back through both moves, under the node service's reader's ring, to the ask
/// as B's push wrote it, and names it by its offset on B.
/// NEGATIVE CONTROL: [`ONE_HOP`]: the receipt follows the copy back one move alone, and names it
/// by the first copy's offset on A.
#[test]
fn mail_unread_across_two_rotations_reaches_its_session_once() {
    use crate::federate::tests::routes_as;
    for control in [false, true] {
        let case = format!("control={control}");
        let (ma, mb) = (Mac::new(65), Mac::new(66));
        let (mid, last) = (ma.rotated(67), ma.rotated(68));
        let lane_of = |node: &str| format!("/f/{FLEET}/in/{node}/s-a/{}/ask", mb.node);
        let held = |node: &str| -> Vec<u64> {
            ma.holds(&lane_of(node))
                .into_iter()
                .map(|(off, _, _, _)| off)
                .collect()
        };
        {
            let mut push = ma.client(
                mb.key,
                &[format!(
                    "rw,p={0}:/f/{FLEET}/in/{1}/*/{0}/*",
                    mb.node, ma.node
                )],
            );
            let mut body = Body::new(0);
            body.from = Some("s-b".to_owned());
            body.text = "unread twice".to_owned();
            push.publish(pid(&mb.node), 7, &lane_of(&ma.node), &body.encode())
                .expect("B's push lands");
        }
        let net = Arc::new(Net::default());
        let handoff = |me: &Mac, olds: &[&Mac]| Handoff {
            me: me.node.clone(),
            members: [me.node.clone(), mb.node.clone()].into(),
            moved: olds
                .iter()
                .map(|old| (old.node.clone(), me.node.clone()))
                .collect(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        };
        let generation = |me: &Mac, olds: &[&Mac]| {
            let routes = routes_as(
                &ma,
                (&me.node, me.key),
                &olds.iter().map(|old| old.node.clone()).collect(),
                vec![Peer::of(&mb)],
                &net,
                false,
            );
            routes.set(&handoff(me, olds), &BTreeSet::new());
            routes
        };
        // The first rotation, and the node service under `mid` moves the ask to it.
        let routes = generation(&mid, &[&ma]);
        until(&format!("{case}: the first move"), || {
            held(&mid.node).len() == 1
        });
        drop(routes);
        // The second, before any bridge of `mid` read it.
        let routes = generation(&last, &[&ma, &mid]);
        until(&format!("{case}: the second move"), || {
            held(&last.node).len() == 1
        });
        let second = handoff(&last, &[&ma, &mid]);
        let keys = keys_for(68, Some(&second));
        crate::federate::write_old_reads(keys.path(), &second, &|grant| {
            Mac::mint(&ma.secret, grant)
        })
        .expect("the node service's reader's ring");
        let (lane, mut a, _served) =
            bridge_on(&last, "s-a", keys.path(), None, crate::fx::Fx::os());
        a.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
        assert!(
            a.attach_broker(),
            "{case}: the bridge attaches to its bus.log"
        );
        let copy = held(&last.node)[0];
        rounds_until(&format!("{case}: the bridge delivers"), &mut a, || {
            lane.delivery_offsets().contains(&copy)
        });
        assert_eq!(lane.delivery_offsets(), [copy], "{case}: once");
        let first = format!("was={}", held(&ma.node)[0]);
        assert!(
            lane.deliveries()
                .iter()
                .any(|line| line.split_whitespace().any(|tok| tok == first)),
            "{case}: the copy names the ask's first offset on A, through both moves: {:?}",
            lane.deliveries()
        );
        ONE_HOP.with(|on| on.set(control));
        let sent = a.send_receipt(&OwedReceipt {
            sid: "s-a".to_owned(),
            rid: 1,
            off: copy,
            verdict: "handled".to_owned(),
            kind: "ask".to_owned(),
            from: format!("s-b@{}", mb.node),
            earlier_log: false,
        });
        ONE_HOP.with(|on| on.set(false));
        sent.expect("the receipt goes");
        let ack = format!("/f/{FLEET}/in/{}/s-b/{}/ack", mb.node, last.node);
        assert_eq!(
            ma.holds(&ack)
                .iter()
                .map(|(_, _, body, _)| Body::decode(body).0.re)
                .collect::<Vec<_>>(),
            [Some(if control { held(&mid.node)[0] } else { 7 })],
            "{case}: the receipt names the ask where it came from"
        );
        drop(routes);
    }
}

/// **AN ASK TO A PERSON UNREAD ACROSS TWO ROTATIONS NAMES ITSELF AS ITS SENDER'S MAC DOES** (§11
/// C3d): B's push brings `s-b`'s ask to `h-andrew` to A's lane back to the person, and A's key
/// rotates twice before A's bridge reads it. The node service moves it to each new node, and A's
/// bridge under the second hands it to the person's lane here once, stamped with its offset on B
/// and B's producer ([`crate::person::WIRE_RE_TOKEN`]), followed back through both moves as a
/// session's answer names what it answers ([`Bridge::wire_re`]), so the person's answer ends
/// `s-b`'s wait. The ask brings a stamp of its own, which is not the bridge's: it is dropped.
/// NEGATIVE CONTROL: [`ONE_HOP`]: the copy is followed back one move alone, to a record the first
/// move wrote, whose origin names no offset on B: the copy carries no stamp, neither the bridge's
/// nor the one the ask brought.
#[test]
fn an_ask_to_a_person_unread_across_two_rotations_names_itself_as_its_senders_mac_does() {
    use crate::federate::tests::routes_as;
    use crate::person::WIRE_RE_TOKEN;
    for control in [false, true] {
        let case = format!("control={control}");
        let (ma, mb) = (Mac::new(73), Mac::new(74));
        let (mid, last) = (ma.rotated(75), ma.rotated(76));
        let lane_of = |node: &str| format!("/f/{FLEET}/in/{node}/h-andrew/{}/ask", mb.node);
        let held = |node: &str| -> Vec<u64> {
            ma.holds(&lane_of(node))
                .into_iter()
                .map(|(off, _, _, _)| off)
                .collect()
        };
        {
            let mut push = ma.client(
                mb.key,
                &[format!(
                    "rw,p={0}:/f/{FLEET}/in/{1}/*/{0}/*",
                    mb.node, ma.node
                )],
            );
            let mut body = Body::new(0);
            body.from = Some("s-b".to_owned());
            body.text = "ship it?".to_owned();
            body.unknown.insert(
                WIRE_RE_TOKEN.to_owned(),
                crate::person::wire_stamp(5, pid(&mb.node)),
            );
            push.publish(pid(&mb.node), 7, &lane_of(&ma.node), &body.encode())
                .expect("B's push lands");
        }
        let net = Arc::new(Net::default());
        let handoff = |me: &Mac, olds: &[&Mac]| Handoff {
            me: me.node.clone(),
            members: [me.node.clone(), mb.node.clone()].into(),
            moved: olds
                .iter()
                .map(|old| (old.node.clone(), me.node.clone()))
                .collect(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        };
        let generation = |me: &Mac, olds: &[&Mac]| {
            let routes = routes_as(
                &ma,
                (&me.node, me.key),
                &olds.iter().map(|old| old.node.clone()).collect(),
                vec![Peer::of(&mb)],
                &net,
                false,
            );
            routes.set(&handoff(me, olds), &BTreeSet::new());
            routes
        };
        let routes = generation(&mid, &[&ma]);
        until(&format!("{case}: the first move"), || {
            held(&mid.node).len() == 1
        });
        drop(routes);
        let routes = generation(&last, &[&ma, &mid]);
        until(&format!("{case}: the second move"), || {
            held(&last.node).len() == 1
        });
        let second = handoff(&last, &[&ma, &mid]);
        let keys = keys_for(76, Some(&second));
        crate::federate::write_old_reads(keys.path(), &second, &|grant| {
            Mac::mint(&ma.secret, grant)
        })
        .expect("the node service's reader's ring");
        let (_lane, mut a, _served) =
            bridge_on(&last, "s-a", keys.path(), None, crate::fx::Fx::os());
        a.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
        assert!(
            a.attach_broker(),
            "{case}: the bridge attaches to its bus.log"
        );
        let person = format!("/f/{FLEET}/in/p/h-andrew/{}/>", last.node);
        ONE_HOP.with(|on| on.set(control));
        rounds_until(
            &format!("{case}: the bridge hands it to the person"),
            &mut a,
            || !ma.holds(&person).is_empty(),
        );
        ONE_HOP.with(|on| on.set(false));
        let handed = ma.holds(&person);
        assert_eq!(handed.len(), 1, "{case}: once");
        let body = Body::decode(&handed[0].2).0;
        assert_eq!(
            body.from.as_deref(),
            Some(format!("s-b@{}", mb.node).as_str()),
            "{case}"
        );
        assert_eq!(
            body.unknown.get(WIRE_RE_TOKEN).cloned(),
            (!control).then(|| crate::person::wire_stamp(7, pid(&mb.node))),
            "{case}: the ask's offset on B and B's producer, through both moves"
        );
        drop(routes);
    }
}

/// **AN ASK FROM A MAC THAT ROTATED ITS KEY IS STAMPED BY THE PUSH THAT BROUGHT IT** (§11 C3d):
/// Q's session asked `h-andrew` under Q's first node, and Q's key rotated before its push
/// carried the ask, so the push under Q's next node brings it to P's lane back to the person
/// from the first. P has no hand-off yet; P's bridge hands the ask to the person stamped with its
/// offset on Q and the producer of the push, with no hand-off read, and the person's answer maps
/// to that offset once a hand-off says where Q's first node's seat is now, also after a second
/// rotation of Q's key.
/// NEGATIVE CONTROL: [`STAMP_BY_SENDER_SEQ`], the stamp by the hand-off the bridge reads
/// ([`Bridge::sender_seq`]), as before: with none, the copy carries no stamp.
#[test]
fn an_ask_from_a_mac_that_rotated_its_key_is_stamped_by_the_push_that_brought_it() {
    use crate::person::{wire_stamp, WIRE_RE_TOKEN};
    for control in [false, true] {
        let case = format!("control={control}");
        let (mp, first) = (Mac::new(83), Mac::new(84));
        let (second, third) = (first.rotated(85), first.rotated(86));
        let asked = format!("/f/{FLEET}/in/{}/h-andrew/{}/ask", mp.node, first.node);
        {
            let mut push = mp.client(
                second.key,
                &[format!(
                    "rw,p={}:/f/{FLEET}/in/{}/*/{}/*",
                    second.node, mp.node, first.node
                )],
            );
            let mut body = Body::new(0);
            body.from = Some("s-q".to_owned());
            body.text = "ship it?".to_owned();
            push.publish(pid(&second.node), 7, &asked, &body.encode())
                .expect("Q's push under its next node lands");
        }
        let keys = keys_for(83, None);
        let (_lane, mut p, _served) = bridge_on(&mp, "s-p", keys.path(), None, crate::fx::Fx::os());
        assert!(p.attach_broker(), "{case}: the bridge attaches");
        let person = format!("/f/{FLEET}/in/p/h-andrew/{}/>", mp.node);
        STAMP_BY_SENDER_SEQ.with(|on| on.set(control));
        rounds_until(
            &format!("{case}: the ask is handed to the person"),
            &mut p,
            || !mp.holds(&person).is_empty(),
        );
        STAMP_BY_SENDER_SEQ.with(|on| on.set(false));
        let handed = mp.holds(&person);
        assert_eq!(handed.len(), 1, "{case}: once");
        let (copy, _, raw, _) = &handed[0];
        let body = Body::decode(raw).0;
        assert_eq!(
            body.from.as_deref(),
            Some(format!("s-q@{}", first.node).as_str()),
            "{case}: from the session under the node it asked from"
        );
        assert_eq!(
            body.unknown.get(WIRE_RE_TOKEN).cloned(),
            (!control).then(|| wire_stamp(7, pid(&second.node))),
            "{case}: the ask's offset on Q, and the producer of the push that brought it"
        );
        let mut reader = mp.client(mp.key, &[format!("ro:/f/{FLEET}/in/p/h-andrew/>")]);
        for (now, moved) in [
            (&second, vec![(first.node.clone(), second.node.clone())]),
            (
                &third,
                vec![
                    (first.node.clone(), second.node.clone()),
                    (second.node.clone(), third.node.clone()),
                ],
            ),
        ] {
            let handoff = Handoff {
                me: mp.node.clone(),
                members: [mp.node.clone(), now.node.clone()].into(),
                moved: moved.into_iter().collect(),
                rank: BTreeMap::new(),
                gone: BTreeSet::new(),
            };
            assert_eq!(
                crate::person::wire_re(
                    &mut reader,
                    (FLEET, "h-andrew"),
                    &handoff,
                    ("s-q", &now.node),
                    *copy,
                )
                .expect("the person's lanes read"),
                (!control).then_some(7),
                "{case}: the person's answer to s-q on Q, now {}",
                now.node
            );
        }
    }
}

/// **A RECORD THE OLD NODE'S BRIDGE DELIVERED DOES NOT MOVE** (§11 C3b): A's bridge as its old
/// node delivered B's note and had not committed it when A's node key rotated. The node service's
/// move of the old node's unread mail waits while that bridge may still deliver, its state dir
/// naming the old node. The bridge goes with no broker to commit on and hands its commit over;
/// A's bridge as the new node starts on the same state dir, and the move, open now, makes that
/// commit first: it moves B's later ask alone, which reaches `s-a` once, and the note reaches it
/// no second time.
/// NEGATIVE CONTROL: [`crate::federate::UNGATED_INBOX`]: the move opens at once, while the old
/// node's bridge still runs, and moves the note that bridge delivered, which the new node's
/// bridge delivers again.
#[test]
fn a_record_the_old_nodes_bridge_delivered_does_not_move() {
    use crate::federate::{mail_group, tests::routes_as, UNGATED_INBOX};
    for control in [false, true] {
        let case = format!("control={control}");
        let (ma, mb) = (Mac::new(81), Mac::new(82));
        let new = ma.rotated(83);
        let before = members(&ma, &[&ma.node, &mb.node]);
        let after = Handoff {
            me: new.node.clone(),
            members: [new.node.clone(), mb.node.clone()].into(),
            moved: [(ma.node.clone(), new.node.clone())].into(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        };
        // B's mail for `s-a` at `dst`, as B's push writes it on A's log.
        let from_b = |dst: &str, kind: &str, on_b: u64, text: &str| -> u64 {
            let subject = format!("/f/{FLEET}/in/{dst}/s-a/{}/{kind}", mb.node);
            let mut push = ma.client(
                mb.key,
                &[format!(
                    "rw,p={}:/f/{FLEET}/in/{dst}/*/{}/*",
                    mb.node, mb.node
                )],
            );
            let mut body = Body::new(0);
            body.from = Some("s-b".to_owned());
            body.text = text.to_owned();
            push.publish(pid(&mb.node), on_b, &subject, &body.encode())
                .expect("B's push lands")
                .0
        };
        let state = ma.link_state();

        // A's bridge as the old node, on the state dir aterm gives it beside A's bus.log, takes
        // B's note: the endpoint has it, and the bridge holds its group's commit for the burst.
        let k_old = keys_for(81, Some(&before));
        let (lane_old, mut old, served_old) =
            bridge_on(&ma, "s-a", k_old.path(), Some(&state), crate::fx::Fx::os());
        assert!(
            old.attach_broker(),
            "{case}: the old node's bridge attaches"
        );
        let read = from_b(&ma.node, "note", 3, "read before");
        let note_lane = format!("/f/{FLEET}/in/{}/s-a/{}/note", ma.node, mb.node);
        let rec: BrokerRecord = ma
            .holds(&note_lane)
            .into_iter()
            .find(|(off, ..)| *off == read)
            .map(|(off, subject, raw, _)| (off, subject, raw))
            .expect("B's note on A's log");
        old.in_round = true;
        old.on_inbox_record(&rec, None);
        old.in_round = false;
        assert_eq!(lane_old.delivery_offsets(), [read], "{case}: delivered");
        assert!(
            ma.broker.group_start(&mail_group(&ma.node)) <= read,
            "{case}: PRECONDITION: its commit is held"
        );
        let ask = from_b(&ma.node, "ask", 7, "unread");

        // A's node key rotates while that bridge still runs: the node service's next generation
        // writes its reader's ring and the hand-off, then starts its pushes.
        let k_new = keys_for(83, Some(&after));
        crate::federate::write_old_reads(k_new.path(), &after, &|grant| {
            Mac::mint(&ma.secret, grant)
        })
        .expect("the node service's reader's ring");
        let net = Arc::new(Net::default());
        UNGATED_INBOX.with(|on| on.set(control));
        let routes = routes_as(
            &ma,
            (&new.node, new.key),
            &BTreeSet::from([ma.node.clone()]),
            vec![Peer::of(&mb)],
            &net,
            false,
        );
        UNGATED_INBOX.with(|on| on.set(false));
        routes.set(&after, &BTreeSet::new());
        let inbox = Lane::Inbox {
            old: ma.node.clone(),
        };
        let moved_note = format!("/f/{FLEET}/in/{}/s-a/{}/note", new.node, mb.node);
        let moved_ask = format!("/f/{FLEET}/in/{}/s-a/{}/ask", new.node, mb.node);
        if control {
            until(
                &format!("{case}: the move takes the note and the ask"),
                || new.holds(&moved_note).len() == 1 && new.holds(&moved_ask).len() == 1,
            );
        } else {
            until(
                &format!("{case}: the move waits for the old node's bridge"),
                || {
                    routes
                        .status()
                        .get(&inbox)
                        .and_then(|status| status.last_error.clone())
                        .is_some_and(|why| why.contains("may still deliver"))
                },
            );
            assert!(
                new.holds(&moved_note).is_empty() && new.holds(&moved_ask).is_empty(),
                "{case}: nothing moved while it waits"
            );
        }

        // The old node's bridge goes with no broker to commit on, handing its commit over.
        old.state.set_seen_off("s-a", read).expect("seen");
        old.conn = None;
        old.wind_down();
        assert_eq!(
            crate::state::StateDir::peek(&state).inbox_owed(&ma.node),
            Some(read),
            "{case}: handed over"
        );
        drop((old, lane_old));

        // A's bridge as the new node starts on the same state dir, naming itself there.
        let (lane, mut a, _served) =
            bridge_on(&new, "s-a", k_new.path(), Some(&state), crate::fx::Fx::os());
        assert!(a.attach_broker(), "{case}: the new node's bridge attaches");
        // In place of the push's next back-off (at most BACKOFF_MAX), which a one-minute hang
        // detector would race: nothing in production kicks the move as the bridge names itself.
        routes.kick();
        until(&format!("{case}: the ask moves"), || {
            new.holds(&moved_ask).len() == 1
        });
        let asked = new.holds(&moved_ask)[0].0;
        rounds_until(
            &format!("{case}: the new node's bridge delivers the ask"),
            &mut a,
            || lane.delivery_offsets().contains(&asked),
        );
        let notes: Vec<u64> = new
            .holds(&moved_note)
            .into_iter()
            .map(|(off, ..)| off)
            .collect();
        if control {
            assert_eq!(
                lane.delivery_offsets(),
                [notes[0], asked],
                "{case}: the note the old bridge delivered reaches s-a again"
            );
        } else {
            assert_eq!(notes, [], "{case}: the note never moves");
            assert_eq!(
                lane.delivery_offsets(),
                [asked],
                "{case}: the ask alone, once"
            );
            until(&format!("{case}: the old group passes both"), || {
                ma.broker.group_start(&mail_group(&ma.node)) > ask
            });
        }
        drop((routes, served_old));
    }
}

/// **A MOVED COPY NAMES WHERE ITS MAIL WAS FIRST** (§11 C3b): A's bridge as its old node delivered
/// B's ask and was killed before it committed or handed anything over, so the move, once A's
/// bridge runs as the new node on the same state dir, moves the ask that bridge delivered. The
/// new node's bridge delivers the copy naming the offset the ask was first at (`was=`), from
/// which aterm answers it as the row the ask got before (`aterm_gui_fabric`'s
/// `a_moved_copy_of_mail_shown_before_the_move_is_not_shown_again`), so `s-a` is not shown it
/// again: whether the copy comes live, the bridge attached as the move opens, or in the refill of
/// a bridge that attaches once the move has run. The bridge knows the copy for one by the origin
/// the read that brought it stamped, and reads the bus only to follow a copy back: B's fresh
/// mail to the new node is delivered with no read of its own. And the row the old bridge
/// delivered stays on the old node's lane, where the new bridge reads it: its receipt names the
/// ask by its offset on B, and its body is fetched from there.
/// NEGATIVE CONTROLS: an endpoint on a lane before [`aterm_types::control_verbs::LINK_MOVED_LANE`]:
/// the copy names no first offset, and aterm shows it as new mail; [`REFILL_UNSTAMPED`]: the
/// refill's copy, taken as stamped with no origin, names none either; and [`OWN_LANES_ONLY`]:
/// the old row's receipt names it by its offset on A, and its body is no record.
#[test]
fn a_moved_copy_of_what_a_killed_bridge_delivered_names_where_it_was_first() {
    use crate::federate::{mail_group, tests::routes_as};
    use aterm_types::control_verbs::{BRIDGE_LANE, LINK_MOVED_LANE};
    for (case, refill, names) in [
        ("live", false, true),
        ("an older lane", false, false),
        ("refill", true, true),
        ("an unstamped refill", true, false),
    ] {
        let (ma, mb) = (Mac::new(84), Mac::new(85));
        let new = ma.rotated(86);
        let before = members(&ma, &[&ma.node, &mb.node]);
        let after = Handoff {
            me: new.node.clone(),
            members: [new.node.clone(), mb.node.clone()].into(),
            moved: [(ma.node.clone(), new.node.clone())].into(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        };
        let lane_on = |node: &str| format!("/f/{FLEET}/in/{node}/s-a/{}/ask", mb.node);
        let from_b = |dst: &str, on_b: u64, text: &str| -> u64 {
            let mut push = ma.client(
                mb.key,
                &[format!(
                    "rw,p={}:/f/{FLEET}/in/{dst}/*/{}/*",
                    mb.node, mb.node
                )],
            );
            let mut body = Body::new(0);
            body.from = Some("s-b".to_owned());
            body.text = text.to_owned();
            push.publish(pid(&mb.node), on_b, &lane_on(dst), &body.encode())
                .expect("B's push lands")
                .0
        };
        let read = from_b(&ma.node, 3, "read before");
        let state = ma.link_state();

        // A's bridge as the old node delivers the ask, holding its group's commit, and is
        // killed: no wind-down, so nothing is committed or handed over.
        let k_old = keys_for(84, Some(&before));
        let (lane_old, mut old, served_old) =
            bridge_on(&ma, "s-a", k_old.path(), Some(&state), crate::fx::Fx::os());
        assert!(
            old.attach_broker(),
            "{case}: the old node's bridge attaches"
        );
        let rec: BrokerRecord = ma
            .holds(&lane_on(&ma.node))
            .into_iter()
            .find(|(off, ..)| *off == read)
            .map(|(off, subject, raw, _)| (off, subject, raw))
            .expect("B's ask on A's log");
        old.in_round = true;
        old.on_inbox_record(&rec, None);
        old.in_round = false;
        assert_eq!(lane_old.delivery_offsets(), [read], "{case}: delivered");
        old.conn = None;
        drop((old, lane_old));
        assert_eq!(
            crate::state::StateDir::peek(&state).inbox_owed(&ma.node),
            None,
            "{case}: PRECONDITION: a killed bridge hands nothing over"
        );
        assert!(
            ma.broker.group_start(&mail_group(&ma.node)) <= read,
            "{case}: PRECONDITION: nor committed the ask"
        );

        // A's node key rotates; A's bridge as the new node starts on the same state dir, and the
        // node service's move opens once it names itself there.
        let k_new = keys_for(86, Some(&after));
        crate::federate::write_old_reads(k_new.path(), &after, &|grant| {
            Mac::mint(&ma.secret, grant)
        })
        .expect("the node service's reader's ring");
        let net = Arc::new(Net::default());
        let routes = routes_as(
            &ma,
            (&new.node, new.key),
            &BTreeSet::from([ma.node.clone()]),
            vec![Peer::of(&mb)],
            &net,
            false,
        );
        let (lane, mut a, _served) =
            bridge_on(&new, "s-a", k_new.path(), Some(&state), crate::fx::Fx::os());
        a.endpoint_lane = if case == "an older lane" {
            LINK_MOVED_LANE - 1
        } else {
            BRIDGE_LANE
        };
        let moved = lane_on(&new.node);
        let copy = if refill {
            routes.set(&after, &BTreeSet::new());
            until(&format!("{case}: the move takes the ask"), || {
                new.holds(&moved).len() == 1
            });
            let copy = new.holds(&moved)[0].0;
            REFILL_UNSTAMPED.with(|on| on.set(case == "an unstamped refill"));
            let attached = a.attach_broker();
            REFILL_UNSTAMPED.with(|on| on.set(false));
            assert!(attached, "{case}: the new node's bridge attaches");
            assert_eq!(
                lane.delivery_offsets(),
                [copy],
                "{case}: the attach's refill delivers the copy"
            );
            copy
        } else {
            assert!(a.attach_broker(), "{case}: the new node's bridge attaches");
            routes.set(&after, &BTreeSet::new());
            until(&format!("{case}: the move takes the ask"), || {
                new.holds(&moved).len() == 1
            });
            let copy = new.holds(&moved)[0].0;
            ORIGIN_READS.with(|reads| reads.set(0));
            rounds_until(
                &format!("{case}: the new node's bridge delivers the copy"),
                &mut a,
                || lane.delivery_offsets().contains(&copy),
            );
            if names {
                assert!(
                    ORIGIN_READS.with(std::cell::Cell::get) > 0,
                    "{case}: CONTROL: following the copy back reads the old node's lane"
                );
            }
            copy
        };
        let line = lane
            .deliveries()
            .into_iter()
            .find(|line| line.contains(&format!(" off={copy} ")))
            .expect("the copy's deliver line");
        let first = format!("was={read}");
        assert_eq!(
            line.split_whitespace().any(|tok| tok == first),
            names,
            "{case}: {line}"
        );
        if case != "live" {
            drop((routes, served_old));
            continue;
        }

        // B's mail to the new node is no copy: its origin says so, and nothing is read for it.
        let fresh = from_b(&new.node, 9, "to the new node");
        ORIGIN_READS.with(|reads| reads.set(0));
        rounds_until(
            &format!("{case}: the fresh ask is delivered"),
            &mut a,
            || lane.delivery_offsets().contains(&fresh),
        );
        assert_eq!(
            ORIGIN_READS.with(std::cell::Cell::get),
            0,
            "{case}: no read for mail no move made"
        );

        // The row the old bridge delivered is answered now, through the new node's bridge.
        let receipt = |rid: u64| OwedReceipt {
            sid: "s-a".to_owned(),
            rid,
            off: read,
            verdict: "handled".to_owned(),
            kind: "ask".to_owned(),
            from: format!("s-b@{}", mb.node),
            earlier_log: false,
        };
        OWN_LANES_ONLY.with(|on| on.set(true));
        let sent = a.send_receipt(&receipt(1));
        let fetched = a.fetch_record("s-a", read);
        OWN_LANES_ONLY.with(|on| on.set(false));
        sent.expect("the receipt goes");
        assert!(
            fetched.is_err(),
            "{case}: CONTROL: on this node's lanes alone the old row is no record"
        );
        a.send_receipt(&receipt(2)).expect("the receipt goes");
        let ack = format!("/f/{FLEET}/in/{}/s-b/{}/ack", mb.node, new.node);
        assert_eq!(
            new.holds(&ack)
                .iter()
                .map(|(_, _, body, _)| Body::decode(body).0.re)
                .collect::<Vec<_>>(),
            [Some(read), Some(3)],
            "{case}: read on the old node's lane, the receipt names the ask by its offset on B"
        );
        let (fields, text) = a
            .fetch_record("s-a", read)
            .unwrap_or_else(|_| panic!("{case}: the old row's body is fetched"));
        assert_eq!(text, "read before", "{case}: {fields}");
        assert!(
            fields.contains(&format!("from=s-b@{}", mb.node)),
            "{case}: {fields}"
        );
        drop((routes, served_old));
    }
}

/// The `deliver` lines `lane`'s aterm took, in order: those of the offsets it answered OK (a line it
/// refused, as an endpoint that owes the lane refuses it, is shown nothing and remembered by none).
fn taken(lane: &FakeVerbLane) -> Vec<String> {
    let ok = lane.delivery_offsets();
    lane.deliveries()
        .into_iter()
        .filter(|line| {
            line.split_whitespace()
                .find_map(|tok| tok.strip_prefix("off="))
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|off| ok.contains(&off))
        })
        .collect()
}

/// The offset of each delivery to `sid` among `lines` (the `deliver` lines one aterm took, in
/// order, from every bridge that ran on its Mac: [`taken`]) that aterm shows its session as new mail: each
/// decided as aterm-gui-fabric's `deliver_row` decides it, by
/// [`aterm_mesh_core::inbox_move::shows_anew`] over the offsets that session's inbox remembers,
/// every offset a delivery it showed or answered named (`off=` and `was=`). That aterm keeps that
/// memory is aterm-gui-fabric's (`a_moved_copy_of_mail_shown_before_the_move_is_not_shown_again`).
fn shown_to(lines: &[String], sid: &str) -> Vec<u64> {
    let mut known = BTreeSet::new();
    let mut shown = Vec::new();
    for line in lines {
        let mut words = line.split_whitespace();
        if words.next() != Some("deliver") || words.next() != Some(sid) {
            continue;
        }
        let field = |name: &str| {
            line.split_whitespace()
                .find_map(|tok| tok.strip_prefix(name))
                .and_then(|n| n.parse::<u64>().ok())
        };
        let Some(off) = field("off=") else {
            continue;
        };
        let was = field("was=");
        let (off_known, was_known) = (
            known.contains(&off),
            was.is_some_and(|w| known.contains(&w)),
        );
        if aterm_mesh_core::inbox_move::shows_anew(off_known, was_known) {
            shown.push(off);
        }
        known.insert(off);
        known.extend(was);
    }
    shown
}

/// How the old node's bridge's inbox commit came to stand below a record it then delivered past
/// (§11 C3b's residual duplicates, second class).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Floor {
    /// aterm answered `s-o`'s delivery with no verdict: its lane is owed.
    OwedLane,
    /// A record left unaccounted, as `account_inbox` leaves one aterm's connection lost.
    AtermLost,
    /// The `undeliverable` notice for a record is held for its resend.
    HeldNotice,
}

/// **MAIL A BRIDGE DELIVERED ABOVE ITS HELD COMMIT IS SHOWN ONCE ACROSS THE MOVE** (§11 C3b's
/// residual duplicates, C3e): A's bridge as its old node could not account B's note to `s-o` (its
/// lane owed, aterm's connection lost while it went, or its `undeliverable` notice held), so its
/// inbox commit stands below that note, and then delivered B's ask to `s-a`, above it. It goes,
/// handing over that floor, and A's key rotates. The move carries both from the floor, and the
/// new node's bridge delivers each copy naming where its mail was first (`was=`): aterm shows the
/// ask to `s-a` once, its copy answered from the memory of the ask's first offset, and the note
/// to `s-o`, which no session was shown, once.
/// NEGATIVE CONTROL: an endpoint on a lane before
/// [`aterm_types::control_verbs::LINK_MOVED_LANE`], the path before C3b's residual duplicates were
/// closed: the copy names no first offset, and `s-a` is shown the ask twice.
#[test]
fn mail_delivered_above_a_held_commit_is_shown_once_across_the_move() {
    use crate::federate::{mail_group, tests::routes_as};
    use aterm_types::control_verbs::{BRIDGE_LANE, LINK_MOVED_LANE};
    for (floor, control) in [
        (Floor::OwedLane, false),
        (Floor::AtermLost, false),
        (Floor::HeldNotice, false),
        (Floor::OwedLane, true),
        (Floor::HeldNotice, true),
    ] {
        let case = format!("{floor:?} control={control}");
        let (ma, mb) = (Mac::new(91), Mac::new(92));
        let new = ma.rotated(93);
        let before = members(&ma, &[&ma.node, &mb.node]);
        let after = Handoff {
            me: new.node.clone(),
            members: [new.node.clone(), mb.node.clone()].into(),
            moved: [(ma.node.clone(), new.node.clone())].into(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        };
        let lane_on = |node: &str, sid: &str, kind: &str| {
            format!("/f/{FLEET}/in/{node}/{sid}/{}/{kind}", mb.node)
        };
        let from_b = |sid: &str, kind: &str, on_b: u64, text: &str| -> BrokerRecord {
            let subject = lane_on(&ma.node, sid, kind);
            let mut push = ma.client(
                mb.key,
                &[format!(
                    "rw,p={}:/f/{FLEET}/in/{}/*/{}/*",
                    mb.node, ma.node, mb.node
                )],
            );
            let mut body = Body::new(0);
            body.from = Some("s-b".to_owned());
            body.text = text.to_owned();
            let raw = body.encode();
            let off = push
                .publish(pid(&mb.node), on_b, &subject, &raw)
                .expect("B's push lands")
                .0;
            (off, subject, raw)
        };
        let state = ma.link_state();

        // A's bridge as the old node: the note stays unaccounted, and the ask is delivered.
        let k_old = keys_for(91, Some(&before));
        let (lane_old, mut old, served_old) = bridge_hosting(
            &ma,
            &[(1, "s-a"), (2, "s-o")],
            k_old.path(),
            Some(&state),
            crate::fx::Fx::os(),
        );
        if floor == Floor::OwedLane {
            lane_old.answer_deliveries_to("s-o", SHIPPED_DELIVER_USAGE);
        }
        assert!(
            old.attach_broker(),
            "{case}: the old node's bridge attaches"
        );
        // B's mail lands once the bridge is attached, so it is the bridge's tail that takes it,
        // as each record is handed here.
        let note = from_b("s-o", "note", 3, "held below");
        let ask = from_b("s-a", "ask", 4, "delivered above");
        old.in_round = true;
        match floor {
            Floor::OwedLane => {
                old.on_inbox_record(&note, None);
            }
            Floor::AtermLost => {
                old.commit_floor = Some(note.0);
            }
            Floor::HeldNotice => {
                let notice = format!("/f/{FLEET}/in/{}/s-b/{}/undeliverable", mb.node, ma.node);
                old.notices_held.insert(note.0, (notice, Vec::new()));
            }
        }
        old.on_inbox_record(&ask, None);
        old.in_round = false;
        assert_eq!(
            lane_old.delivery_offsets(),
            [ask.0],
            "{case}: the ask alone delivered"
        );
        assert_eq!(
            old.inbox_commit_floor(),
            Some(note.0),
            "{case}: PRECONDITION: the commit is held at the note"
        );
        old.conn = None;
        old.wind_down();
        assert!(
            ma.broker.group_start(&mail_group(&ma.node)) <= note.0,
            "{case}: PRECONDITION: nothing committed the note"
        );
        assert!(
            crate::state::StateDir::peek(&state)
                .inbox_owed(&ma.node)
                .is_none_or(|owed| owed < note.0),
            "{case}: PRECONDITION: nor is the floor's hand-over past it"
        );
        let old_lines = taken(&lane_old);
        drop((old, lane_old));

        // A's key rotates; the new node's bridge on the same state dir, and the move.
        let k_new = keys_for(93, Some(&after));
        crate::federate::write_old_reads(k_new.path(), &after, &|grant| {
            Mac::mint(&ma.secret, grant)
        })
        .expect("the node service's reader's ring");
        let net = Arc::new(Net::default());
        let routes = routes_as(
            &ma,
            (&new.node, new.key),
            &BTreeSet::from([ma.node.clone()]),
            vec![Peer::of(&mb)],
            &net,
            false,
        );
        let (lane, mut a, _served) = bridge_hosting(
            &new,
            &[(1, "s-a"), (2, "s-o")],
            k_new.path(),
            Some(&state),
            crate::fx::Fx::os(),
        );
        a.endpoint_lane = if control {
            LINK_MOVED_LANE - 1
        } else {
            BRIDGE_LANE
        };
        assert!(a.attach_broker(), "{case}: the new node's bridge attaches");
        routes.set(&after, &BTreeSet::new());
        routes.kick();
        let (moved_note, moved_ask) = (
            lane_on(&new.node, "s-o", "note"),
            lane_on(&new.node, "s-a", "ask"),
        );
        until(&format!("{case}: the move carries both"), || {
            new.holds(&moved_note).len() == 1 && new.holds(&moved_ask).len() == 1
        });
        let (note_copy, ask_copy) = (new.holds(&moved_note)[0].0, new.holds(&moved_ask)[0].0);
        rounds_until(
            &format!("{case}: the new node's bridge delivers both"),
            &mut a,
            || {
                let got = lane.delivery_offsets();
                got.contains(&note_copy) && got.contains(&ask_copy)
            },
        );
        let lines: Vec<String> = old_lines.into_iter().chain(taken(&lane)).collect();
        assert_eq!(
            shown_to(&lines, "s-a"),
            if control {
                vec![ask.0, ask_copy]
            } else {
                vec![ask.0]
            },
            "{case}: the ask: {lines:?}"
        );
        assert_eq!(
            shown_to(&lines, "s-o"),
            [note_copy],
            "{case}: the note, never shown before the move: {lines:?}"
        );
        drop((routes, served_old));
    }
}

/// **A MOVE'S BURST MOVED TWICE ACROSS A SECOND ROTATION IS SHOWN ONCE** (§11 C3b's residual
/// duplicates, C3e): B's ask to `s-a` lies unread on A's `bus.log` for A's first node. Under the
/// second, the node service's move lands it as that node's (its producer, numbered by the ask's
/// offset) and the burst is never acknowledged: the old node's group is not committed past it.
/// A's key rotates again. Under the third node the move of the first node's mail carries the ask
/// again, and the move of the second's carries the first copy on, so A's `bus.log` holds two
/// copies for the third node. Its bridge delivers both, each naming the ask's first offset
/// (`was=`), the second through both moves; aterm shows the ask to `s-a` once. Where the second
/// node's bridge delivered the first copy before the second rotation, aterm answers both later
/// copies from its memory of it.
/// NEGATIVE CONTROL: an endpoint on a lane before
/// [`aterm_types::control_verbs::LINK_MOVED_LANE`]: no copy names a first offset, and `s-a` is
/// shown the ask once per copy.
#[test]
fn a_burst_moved_twice_across_a_second_rotation_is_shown_once() {
    use crate::federate::{mail_group, tests::routes_as};
    use aterm_types::control_verbs::{BRIDGE_LANE, LINK_MOVED_LANE};
    for (mid_delivers, control) in [(false, false), (true, false), (false, true), (true, true)] {
        let case = format!("mid delivers={mid_delivers} control={control}");
        let (ma, mb) = (Mac::new(94), Mac::new(95));
        let (mid, last) = (ma.rotated(96), ma.rotated(97));
        let lane_of = |node: &str| format!("/f/{FLEET}/in/{node}/s-a/{}/ask", mb.node);
        let held = |node: &str| -> Vec<(u64, Vec<u8>)> {
            ma.holds(&lane_of(node))
                .into_iter()
                .map(|(off, _, raw, _)| (off, raw))
                .collect()
        };
        let mut body = Body::new(0);
        body.from = Some("s-b".to_owned());
        body.text = "moved twice".to_owned();
        let raw = body.encode();
        let first = ma
            .client(
                mb.key,
                &[format!(
                    "rw,p={0}:/f/{FLEET}/in/{1}/*/{0}/*",
                    mb.node, ma.node
                )],
            )
            .publish(pid(&mb.node), 7, &lane_of(&ma.node), &raw)
            .expect("B's push lands")
            .0;
        // The second node's move landed the ask, as its push lands a burst (under that node's
        // producer, numbered by the ask's offset), and was never acknowledged: no commit.
        let landed = ma
            .client(
                mid.key,
                &[format!("rw,p={0}:/f/{FLEET}/in/{0}/>", mid.node)],
            )
            .publish(pid(&mid.node), first, &lane_of(&mid.node), &raw)
            .expect("the first move's burst lands")
            .0;
        assert!(
            ma.broker.group_start(&mail_group(&ma.node)) <= first,
            "{case}: PRECONDITION: the first node's group is not committed past the ask"
        );
        let mid_handoff = Handoff {
            me: mid.node.clone(),
            members: [mid.node.clone(), mb.node.clone()].into(),
            moved: [(ma.node.clone(), mid.node.clone())].into(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        };
        let mut lines = Vec::new();
        // The broker outlives a killed bridge: its handle is kept to the end of the case.
        let mut serving = Vec::new();
        if mid_delivers {
            let keys = keys_for(96, Some(&mid_handoff));
            crate::federate::write_old_reads(keys.path(), &mid_handoff, &|grant| {
                Mac::mint(&ma.secret, grant)
            })
            .expect("the node service's reader's ring");
            let (lane, mut m, served) =
                bridge_on(&mid, "s-a", keys.path(), None, crate::fx::Fx::os());
            m.endpoint_lane = if control {
                LINK_MOVED_LANE - 1
            } else {
                BRIDGE_LANE
            };
            assert!(
                m.attach_broker(),
                "{case}: the second node's bridge attaches"
            );
            rounds_until(
                &format!("{case}: the second node's bridge delivers"),
                &mut m,
                || lane.delivery_offsets().contains(&landed),
            );
            // Killed: whatever it committed stands, and nothing more.
            m.conn = None;
            drop(m);
            serving.push(served);
            lines.extend(taken(&lane));
        }
        // The second node's move carries the first copy on only where its bridge's commit stayed
        // below it.
        let mid_moves = ma.broker.group_start(&mail_group(&mid.node)) <= landed;

        // The second rotation: the third node's generation moves both nodes' unread mail.
        let second = Handoff {
            me: last.node.clone(),
            members: [last.node.clone(), mb.node.clone()].into(),
            moved: [
                (ma.node.clone(), last.node.clone()),
                (mid.node.clone(), last.node.clone()),
            ]
            .into(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        };
        let net = Arc::new(Net::default());
        let routes = routes_as(
            &ma,
            (&last.node, last.key),
            &BTreeSet::from([ma.node.clone(), mid.node.clone()]),
            vec![Peer::of(&mb)],
            &net,
            false,
        );
        routes.set(&second, &BTreeSet::new());
        until(&format!("{case}: the moves carry their copies"), || {
            held(&last.node).len() == 1 + usize::from(mid_moves)
        });
        let copies: Vec<u64> = held(&last.node).into_iter().map(|(off, _)| off).collect();
        let keys = keys_for(97, Some(&second));
        crate::federate::write_old_reads(keys.path(), &second, &|grant| {
            Mac::mint(&ma.secret, grant)
        })
        .expect("the node service's reader's ring");
        let (lane, mut a, _served) =
            bridge_on(&last, "s-a", keys.path(), None, crate::fx::Fx::os());
        a.endpoint_lane = if control {
            LINK_MOVED_LANE - 1
        } else {
            BRIDGE_LANE
        };
        assert!(
            a.attach_broker(),
            "{case}: the third node's bridge attaches"
        );
        rounds_until(
            &format!("{case}: the third node's bridge delivers both"),
            &mut a,
            || {
                let got = lane.delivery_offsets();
                copies.iter().all(|copy| got.contains(copy))
            },
        );
        lines.extend(taken(&lane));
        if !control {
            let named = format!("was={first}");
            for copy in &copies {
                let line = lines
                    .iter()
                    .find(|line| line.contains(&format!(" off={copy} ")))
                    .expect("the copy's deliver line");
                assert!(
                    line.split_whitespace().any(|tok| tok == named),
                    "{case}: each copy names the ask's first offset: {line}"
                );
            }
        }
        let want = match (control, mid_delivers) {
            (false, true) => vec![landed],
            (false, false) => vec![copies[0]],
            (true, true) => std::iter::once(landed)
                .chain(copies.iter().copied())
                .collect(),
            (true, false) => copies.clone(),
        };
        assert!(
            mid_delivers || copies.len() == 2,
            "{case}: PRECONDITION: the burst moves twice"
        );
        assert_eq!(shown_to(&lines, "s-a"), want, "{case}: {lines:?}");
        drop((routes, serving));
    }
}

/// **AN OLD NODE'S ROW HELD BELOW ITS LANE'S OLDEST IS READ THERE** (§11 C3b, finding F3): B's ask
/// lies on the lane of A's old node, where a bridge of that node delivered it before A's key
/// rotated. A cut carries it below the base, and the rules set next take its class whole from the
/// base alone, so the old lane's oldest lies above the record the broker still holds. The new
/// node's bridge reads it there again told: its receipt names the ask by its offset on B, and its
/// body is fetched.
/// NEGATIVE CONTROL: `Mutant::ReadAtWithoutTheHeld`, the refusal taken for no record: the receipt
/// names the ask by its offset on A, and its fetch is answered `retained`.
#[test]
fn an_old_nodes_row_held_below_its_lanes_oldest_is_read_there() {
    for control in [false, true] {
        let (ma, mb) = (Mac::new(87), Mac::new(88));
        let new = ma.rotated(89);
        ma.broker
            .set_retention(super::lanes_whole_from_the_base(true))
            .expect("the rules before the cut");
        let after = Handoff {
            me: new.node.clone(),
            members: [new.node.clone(), mb.node.clone()].into(),
            moved: [(ma.node.clone(), new.node.clone())].into(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        };
        let mut push = ma.client(
            mb.key,
            &[format!(
                "rw,p={}:/f/{FLEET}/in/{}/*/{}/*",
                mb.node, ma.node, mb.node
            )],
        );
        let lane = format!("/f/{FLEET}/in/{}/s-a/{}/ask", ma.node, mb.node);
        let mut publish = |on_b: u64, text: &str| -> u64 {
            let mut body = Body::new(0);
            body.from = Some("s-b".to_owned());
            body.text = text.to_owned();
            push.publish(pid(&mb.node), on_b, &lane, &body.encode())
                .expect("B's push lands")
                .0
        };
        let read = publish(3, "read before");
        let later = publish(4, "read after");
        ma.broker
            .retain_before_carrying_state(later + 1)
            .expect("the cut");
        ma.broker
            .set_retention(super::lanes_whole_from_the_base(false))
            .expect("the rules after the cut");
        let old_lane =
            astream_wire::Filter::new(format!("/f/{FLEET}/in/{}/s-a/>", ma.node)).unwrap();
        assert!(
            ma.broker.oldest(&old_lane) > read,
            "PRECONDITION: the old lane's oldest lies above the record the broker holds"
        );

        let keys = keys_for(89, Some(&after));
        crate::federate::write_old_reads(keys.path(), &after, &|grant| {
            Mac::mint(&ma.secret, grant)
        })
        .expect("the node service's reader's ring");
        let fx = if control {
            crate::fx::Fx::os_without(crate::sim::world::Mutant::ReadAtWithoutTheHeld)
        } else {
            crate::fx::Fx::os()
        };
        let (_lane, mut a, _served) = bridge_on(&new, "s-a", keys.path(), None, fx);
        assert!(a.attach_broker(), "the new node's bridge attaches");
        a.send_receipt(&OwedReceipt {
            sid: "s-a".to_owned(),
            rid: 1,
            off: read,
            verdict: "handled".to_owned(),
            kind: "ask".to_owned(),
            from: format!("s-b@{}", mb.node),
            earlier_log: false,
        })
        .expect("the receipt goes");
        let ack = format!("/f/{FLEET}/in/{}/s-b/{}/ack", mb.node, new.node);
        let re: Vec<_> = new
            .holds(&ack)
            .iter()
            .map(|(_, _, body, _)| Body::decode(body).0.re)
            .collect();
        let fetched = a.fetch_record("s-a", read);
        if control {
            assert_eq!(re, [Some(read)], "CONTROL: named by its offset on A");
            assert!(
                matches!(fetched, Err(Unfetched::Retained(_))),
                "CONTROL: answered `retained` for the record the broker holds: {fetched:?}"
            );
        } else {
            assert_eq!(
                re,
                [Some(3)],
                "the receipt names the ask by its offset on B"
            );
            let (fields, text) = fetched.expect("the old row's body is fetched");
            assert_eq!(text, "read before", "{fields}");
        }
    }
}

/// **A REPLY FROM WHERE A ROTATION MOVED THE ASKED NODE IS NO STRAY** (§11 C3b): an ask that went
/// to a node a key rotation retired since, another Mac's or this Mac's own, is answered from the
/// node its seat holds now, and the bridge takes that reply as from where the ask went; a reply
/// from any other node is a stray all the same.
/// NEGATIVE CONTROL: a hand-off that names no rotation: both replies are strays.
#[test]
fn a_reply_from_where_a_rotation_moved_the_asked_node_is_no_stray() {
    let ma = Mac::new(55);
    let (peer_was, peer_now) = (node_of(&key_of(56)), node_of(&key_of(57)));
    let mine_was = node_of(&key_of(58));
    let other = node_of(&key_of(59));
    let keys = keys_for(55, None);
    let (_lane, mut a, _served) = attached(&ma, "s-a", keys.path());
    a.note_asked(5, &peer_was);
    a.note_asked(6, &mine_was);
    for rotated in [true, false] {
        let case = format!("rotated={rotated}");
        Handoff {
            me: ma.node.clone(),
            members: [ma.node.clone(), peer_now.clone(), other.clone()].into(),
            moved: if rotated {
                [
                    (peer_was.clone(), peer_now.clone()),
                    (mine_was.clone(), ma.node.clone()),
                ]
                .into()
            } else {
                BTreeMap::new()
            },
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        }
        .write(keys.path())
        .expect("the hand-off");
        assert_eq!(
            a.reply_from(5, &peer_now),
            Some(rotated),
            "{case}: from the peer's node now"
        );
        let own = a.node.clone();
        assert_eq!(
            a.reply_from(6, &own),
            Some(rotated),
            "{case}: from this Mac's node now"
        );
        assert_eq!(a.reply_from(5, &other), Some(false), "{case}: a third node");
    }
}

/// How a test's person seals a post (§11 C3c).
#[derive(Clone, Copy)]
enum Seal<'a> {
    /// By the person key in the Mac's own folder, under its certificate.
    Own,
    /// Not at all.
    Bare,
    /// By the person key in another folder, under the Mac's own certificate.
    By(&'a std::path::Path),
    /// By the person key in another folder, under that folder's certificate: another Mac's.
    Elsewhere(&'a std::path::Path),
}

/// A post of h-andrew on `mac` to session `s-q` of `dst`, as `aterm fabric post` writes one with
/// the mesh on (§11 C3c): on the person's own lane, under the person's producer at `seq`, sealed
/// as `seal` says. Its offset on `mac`'s log.
fn person_posts(
    mac: &Mac,
    keys: &std::path::Path,
    post: (&str, u64, &str, &str),
    seal: Seal<'_>,
) -> u64 {
    person_posts_re(mac, keys, post, None, seal)
}

/// [`person_posts`], naming `re`.
fn person_posts_re(
    mac: &Mac,
    keys: &std::path::Path,
    (dst, seq, kind, text): (&str, u64, &str, &str),
    re: Option<u64>,
    seal: Seal<'_>,
) -> u64 {
    use crate::person_key::{post_address, read_cert, PersonKey};
    let person = "h-andrew";
    let lane = subject::post_lane(FLEET, dst, "s-q", person, kind);
    let mut body = Body::new(crate::now_ms());
    body.text = text.to_owned();
    body.re = re;
    let sealer = match seal {
        Seal::Own => Some((keys, keys)),
        Seal::Bare => None,
        Seal::By(other) => Some((other, keys)),
        Seal::Elsewhere(other) => Some((other, other)),
    };
    if let Some((key_dir, cert_dir)) = sealer {
        let key = PersonKey::read(key_dir).expect("reads").expect("made");
        let cert = read_cert(cert_dir).expect("reads").expect("vouched");
        key.seal(&cert, &post_address(FLEET, "s-q", person, kind), &mut body);
    }
    let mut client = mac.client(
        mac.key,
        &[format!("rw,p={person}:/f/{FLEET}/in/*/*/{person}/*")],
    );
    client
        .publish(
            astream_cap::producer_id_of(person),
            seq,
            &lane,
            &body.encode(),
        )
        .expect("the person's post lands")
        .0
}

/// **A PERSON'S POST REACHES THE SESSION ON ANOTHER MAC ONCE, AS THE PERSON'S** (§11 C3c): a
/// person on P posts an ask to `@s-q@<Q>`, sealed by P's person key under the certificate of P's
/// node key. P's carry verifies the seal and writes the ask on P's own lane to Q, naming the
/// person, the lane keeping its row; P's push carries it to Q's log once; and Q's
/// bridge verifies the seal again, under the certificate's node key, which names P, and hands it
/// to s-q once, `from=h-andrew@<P> kind=ask trust=human`. A note to a node whose seat rotated to
/// Q is carried to Q and handed to s-q the same way. Restarted, the carry carries nothing twice. A post with no seal, one sealed by another Mac's person key under P's certificate, one
/// under another Mac's certificate, and one to a Mac that is no member are not carried: P's carry
/// counts each and says why on the person's lane on P, where the person's inbox reads it
/// (`reason=`), and Q's log holds none of them.
/// NEGATIVE CONTROLS: with [`crate::federate::UNSEALED_CARRY`] P carries every one but the post to
/// no member, and Q's bridge refuses each
/// (`undeliverable … reason=person-unsealed|person-bad-seal|person-not-vouched`), handing s-q
/// none; with [`TRUST_ANY_PERSON`] on Q too, s-q is handed each as a human's.
#[test]
fn a_persons_post_reaches_the_session_on_another_mac_once_as_the_persons() {
    use crate::federate::{CARRIER, NOTICE_REASON, UNSEALED_CARRY};
    use crate::person_key::Refusal;
    for arm in ["checked", "unsealed-carry", "trust-any-person"] {
        let (mp, mq) = (Mac::new(51), Mac::new(52));
        let both = [mp.node.as_str(), mq.node.as_str()];
        let (kp, kq) = (
            keys_for(51, Some(&members(&mp, &both))),
            keys_for(52, Some(&members(&mq, &both))),
        );
        let now = crate::now_ms();
        assert!(crate::person_key::vouch(kp.path(), &[51; 32], now).is_ok());
        // Another Mac's person key and certificate, which P's node key never signed.
        let elsewhere = keys_for(53, None);
        assert!(crate::person_key::vouch(elsewhere.path(), &[53; 32], now).is_ok());
        let stranger = node_of(&key_of(54));
        // Q's seat before its key rotated, as P's hand-off says.
        let retired = node_of(&key_of(55));
        let mut handoff_p = members(&mp, &both);
        handoff_p.moved.insert(retired.clone(), mq.node.clone());
        let (lane_q, mut q, _served_q) = attached(&mq, "s-q", kq.path());
        let net = Arc::new(Net::default());
        let start = || {
            UNSEALED_CARRY.with(|on| on.set(arm != "checked"));
            let routes = routes_of(&mp, vec![Peer::of(&mq)], &net, false);
            UNSEALED_CARRY.with(|on| on.set(false));
            routes.set(&handoff_p, &BTreeSet::new());
            routes
        };
        let from_p = start();
        TRUST_ANY_PERSON.with(|on| on.set(arm == "trust-any-person"));

        let to = |dst: &str, seq, kind, text, seal| {
            person_posts(&mp, kp.path(), (dst, seq, kind, text), seal)
        };
        let genuine = to(&mq.node, 1, "ask", "genuine", Seal::Own);
        let bare = to(&mq.node, 2, "note", "bare", Seal::Bare);
        let forged = to(&mq.node, 3, "note", "forged", Seal::By(elsewhere.path()));
        let alien = to(
            &mq.node,
            4,
            "note",
            "alien",
            Seal::Elsewhere(elsewhere.path()),
        );
        let unmet = to(&stranger, 5, "note", "unmet", Seal::Own);
        to(&retired, 6, "note", "moved", Seal::Own);
        let carried = if arm == "checked" { 2 } else { 5 };
        until(&format!("{arm}: P's carry took each post"), || {
            let status = from_p.carry_status();
            status.carried + status.refused == 6
        });
        let status = from_p.carry_status();
        assert_eq!(
            (status.carried, status.refused),
            (carried, 6 - carried),
            "{arm}: {status:?}"
        );
        let on_p = mp.holds(&format!("/f/{FLEET}/in/{}/s-q/{}/ask", mq.node, mp.node));
        assert_eq!(
            on_p.iter().map(|row| row.3).collect::<Vec<_>>(),
            vec![Some((astream_cap::producer_id_of(CARRIER), genuine))],
            "{arm}: carried once, under the carrier, numbered by the person's post's offset"
        );
        let carried_lanes = crate::federate::mail_lanes(&mq.node, &mp.node);
        let (charged, _) = mp
            .client(mp.key, &[format!("ro:{carried_lanes}")])
            .last(&carried_lanes, "", 64)
            .expect("P's last-value index");
        let rows: BTreeSet<&str> = charged
            .iter()
            .map(|(_, subject, _)| subject.as_str())
            .collect();
        let lanes: Vec<String> = ["ask", "note"]
            .iter()
            .map(|kind| subject::post_lane(FLEET, &mq.node, "s-q", &mp.node, kind))
            .collect();
        assert_eq!(
            rows,
            lanes.iter().map(String::as_str).collect(),
            "{arm}: each lane the carry wrote keeps its row, for a watch that starts again"
        );
        assert!(
            mp.holds(&format!("/f/{FLEET}/in/{stranger}/>")).len() == 1,
            "{arm}: the post to no member stays the person's alone"
        );
        let on_q = format!("/f/{FLEET}/in/{}/s-q/{}/>", mq.node, mp.node);
        until(&format!("{arm}: what P carried reaches Q's log"), || {
            mq.holds(&on_q).len() == carried as usize
        });
        let held = mq.holds(&on_q);
        let by = |raw: &[u8]| {
            Body::decode(raw)
                .0
                .unknown
                .get(crate::person_key::BY_TOKEN)
                .cloned()
        };
        assert!(
            held.iter()
                .all(|(_, _, raw, _)| by(raw).as_deref() == Some("h-andrew")),
            "{arm}: each names the person"
        );
        let notices: Vec<(Option<u64>, Option<String>)> = mp
            .holds(&format!(
                "/f/{FLEET}/in/p/h-andrew/{}/undeliverable",
                mp.node
            ))
            .iter()
            .map(|(_, _, raw, _)| {
                let body = Body::decode(raw).0;
                (body.re, body.unknown.get(NOTICE_REASON).cloned())
            })
            .collect();
        let told = |off: u64, why: &str| (Some(off), Some(why.to_owned()));
        let unmet_told = told(unmet, "fleet-not-member");
        assert_eq!(
            notices,
            if arm == "checked" {
                vec![
                    told(bare, Refusal::Unsealed.token()),
                    told(forged, Refusal::BadSeal.token()),
                    told(alien, Refusal::NotVouched.token()),
                    unmet_told,
                ]
            } else {
                vec![unmet_told]
            },
            "{arm}: the person's inbox on P says why each was not carried"
        );

        let last = held.iter().map(|(off, _, _, _)| *off).max().expect("one");
        assert!(
            offer_live_record(&mut q, last),
            "{arm}: Q's bridge reads them"
        );
        let deliveries = lane_q.deliveries();
        let delivered: Vec<&String> = deliveries
            .iter()
            .filter(|line| line.starts_with("deliver s-q "))
            .collect();
        for (kind, text) in [("ask", "genuine"), ("note", "moved")] {
            let human = format!("from=h-andrew@{} kind={kind} trust=human", mp.node);
            assert_eq!(
                delivered
                    .iter()
                    .filter(|line| line.contains(&human) && line.ends_with(&format!("text={text}")))
                    .count(),
                1,
                "{arm}: the person's {kind}, once, as the person's: {delivered:?}"
            );
        }
        let face = subject::session_face(FLEET, &mq.node, "s-q", "ev");
        let refusals: Vec<String> = evs_on(&q, &face)
            .into_iter()
            .filter(|ev| ev.starts_with("undeliverable ") && ev.contains("reason=person-"))
            .collect();
        match arm {
            "checked" => {
                assert_eq!(delivered.len(), 2, "{arm}: {delivered:?}");
                assert!(refusals.is_empty(), "{arm}: {refusals:?}");
            }
            "unsealed-carry" => {
                assert_eq!(
                    delivered.len(),
                    2,
                    "{arm}: Q handed s-q none of them: {delivered:?}"
                );
                let reasons: Vec<&str> = refusals
                    .iter()
                    .filter_map(|ev| ev.rsplit_once("reason=").map(|(_, why)| why))
                    .collect();
                assert_eq!(
                    reasons,
                    [
                        Refusal::Unsealed.token(),
                        Refusal::BadSeal.token(),
                        Refusal::NotVouched.token()
                    ],
                    "{arm}: {refusals:?}"
                );
            }
            _ => {
                assert_eq!(delivered.len(), 5, "{arm}: {delivered:?}");
                assert!(
                    delivered.iter().all(|line| line.contains(" trust=human")),
                    "{arm}: {delivered:?}"
                );
            }
        }
        TRUST_ANY_PERSON.with(|on| on.set(false));

        // Restarted, the carry resumes past what it carried: a later post is carried, alone.
        drop(from_p);
        let again = start();
        let later = to(&mq.node, 7, "note", "later", Seal::Own);
        until(
            &format!("{arm}: the restarted carry took the later post"),
            || again.carry_status().carried == 1,
        );
        until(&format!("{arm}: the later post reaches Q's log"), || {
            mq.holds(&on_q).len() == carried as usize + 1
        });
        assert_eq!(again.carry_status().refused, 0, "{arm}");
        assert!(later > unmet);
        drop(again);
    }
}

/// **AN ANSWER AND A RECEIPT TO A PERSON'S POST REACH THE PERSON ON THEIR OWN MAC** (§11 C3d): a
/// person on P asks `@s-q@<Q>`; P's carry carries it and Q's bridge hands it to s-q as
/// `from=h-andrew@<P>`. Q's receipt for it, and s-q's answer `to=@h-andrew@<P>`, go on Q's lane
/// to P under the person's name, naming the carried copy by its offset on P, and Q's push carries
/// both to P's log. P's bridge hands each to the person's lane on P, where their inbox reads it
/// as s-q's on Q, once though offered again: `from=s-q@<Q>`, `re=` the offset of the person's own
/// post, and no token that speaks as a person.
/// NEGATIVE CONTROLS: with [`NO_WAY_BACK`] on Q, Q has no lane back, as before C3d: the receipt is
/// journaled `unacked … reason=no-lane`, the answer goes nowhere, and P's log holds nothing from
/// Q for the person; with [`WIRE_RE_TO_PERSON`] on P, P hands both over naming the carried copy,
/// an offset on P that is not the person's post.
#[test]
fn an_answer_and_a_receipt_to_a_persons_post_reach_the_person_on_their_own_mac() {
    for arm in ["checked", "no-way-back", "wire-re"] {
        let (mp, mq) = (Mac::new(61), Mac::new(62));
        let both = [mp.node.as_str(), mq.node.as_str()];
        let (kp, kq) = (
            keys_for(61, Some(&members(&mp, &both))),
            keys_for(62, Some(&members(&mq, &both))),
        );
        assert!(crate::person_key::vouch(kp.path(), &[61; 32], crate::now_ms()).is_ok());
        let (lane_q, mut q, _served_q) = attached(&mq, "s-q", kq.path());
        let (_lane_p, mut p, _served_p) = attached(&mp, "s-p", kp.path());
        let net = Arc::new(Net::default());
        let from_p = pushes(&mp, &[&mq], &net, &members(&mp, &both));
        let to_p = pushes(&mq, &[&mp], &net, &members(&mq, &both));

        let post = person_posts(
            &mp,
            kp.path(),
            (&mq.node, 1, "ask", "which branch?"),
            Seal::Own,
        );
        let ask = format!("/f/{FLEET}/in/{}/s-q/{}/ask", mq.node, mp.node);
        until(&format!("{arm}: the carried ask reaches Q's log"), || {
            !mq.holds(&ask).is_empty()
        });
        let copy = mp.holds(&ask);
        assert_eq!(copy.len(), 1, "{arm}: P carried the ask once");
        let carried_at = copy[0].0;
        assert_ne!(carried_at, post, "{arm}: the copy lies elsewhere on P");
        let at_q = mq.holds(&ask)[0].0;
        assert!(
            offer_live_record(&mut q, at_q),
            "{arm}: Q's bridge reads it"
        );
        let person = format!("h-andrew@{}", mp.node);
        assert!(
            lane_q
                .deliveries()
                .iter()
                .any(|line| line.starts_with("deliver s-q ")
                    && line.contains(&format!(" from={person} kind=ask trust=human"))),
            "{arm}: s-q was handed the person's ask: {:?}",
            lane_q.deliveries()
        );

        NO_WAY_BACK.with(|on| on.set(arm == "no-way-back"));
        q.send_receipt(&OwedReceipt {
            sid: "s-q".to_owned(),
            rid: 1,
            off: at_q,
            verdict: "handled".to_owned(),
            kind: "ask".to_owned(),
            from: person.clone(),
            earlier_log: false,
        })
        .expect("the receipt goes");
        let text = "main has it";
        *lane_q.outbox.lock().unwrap() = Some(format!(
            "post sid=s-q id=1 to=@{person} kind=answer re={at_q} len={}\n{text}",
            text.len()
        ));
        q.drain_outbox();
        NO_WAY_BACK.with(|on| on.set(false));

        let back = format!("/f/{FLEET}/in/{}/h-andrew/{}/>", mp.node, mq.node);
        let handed = format!("/f/{FLEET}/in/p/h-andrew/{}/>", mp.node);
        if arm == "no-way-back" {
            let face = subject::session_face(FLEET, &mq.node, "s-q", "ev");
            assert!(
                evs_on(&q, &face)
                    .iter()
                    .any(|ev| ev == &format!("unacked re={at_q} verdict=handled reason=no-lane")),
                "{arm}: Q journals the receipt unsent: {:?}",
                evs_on(&q, &face)
            );
            assert!(mq.holds(&back).is_empty(), "{arm}: Q wrote nothing back");
            assert!(
                mp.holds(&handed).is_empty(),
                "{arm}: the person got nothing"
            );
            drop((from_p, to_p));
            continue;
        }
        until(
            &format!("{arm}: the receipt and the answer reach P's log"),
            || mp.holds(&back).len() == 2,
        );
        let arrived = mp.holds(&back);
        for (_, _, raw, origin) in &arrived {
            assert_eq!(
                Body::decode(raw).0.re,
                Some(carried_at),
                "{arm}: Q names the carried copy by its offset on P"
            );
            assert_eq!(
                origin.map(|(producer, _)| producer),
                Some(pid(&mq.node)),
                "{arm}: under Q's producer"
            );
        }
        WIRE_RE_TO_PERSON.with(|on| on.set(arm == "wire-re"));
        let last = arrived.iter().map(|row| row.0).max().expect("two");
        assert!(
            offer_live_record(&mut p, last),
            "{arm}: P's bridge reads them"
        );
        // Offered again, as a replay after a lost commit would: written once.
        for (off, subject, raw, origin) in &arrived {
            p.deliver_record(*off, subject, raw, *origin);
        }
        WIRE_RE_TO_PERSON.with(|on| on.set(false));
        let inbox = mp.holds(&handed);
        let kinds: Vec<&str> = inbox
            .iter()
            .filter_map(|(_, subject, _, _)| subject.rsplit('/').next())
            .collect();
        assert_eq!(kinds, ["ack", "answer"], "{arm}: each handed over once");
        let names = if arm == "wire-re" { carried_at } else { post };
        for (_, _, raw, origin) in &inbox {
            let body = Body::decode(raw).0;
            assert_eq!(
                body.from.as_deref(),
                Some(format!("s-q@{}", mq.node).as_str()),
                "{arm}"
            );
            assert_eq!(body.re, Some(names), "{arm}: names the person's post");
            assert!(
                [
                    crate::person_key::BY_TOKEN,
                    crate::person_key::CERT_TOKEN,
                    crate::person_key::SEAL_TOKEN
                ]
                .iter()
                .all(|token| !body.unknown.contains_key(*token)),
                "{arm}: nothing in it speaks as a person: {:?}",
                body.unknown
            );
            assert_eq!(
                origin.map(|(producer, _)| producer),
                Some(pid(&mp.node)),
                "{arm}: written by P's bridge"
            );
        }
        let ack = Body::decode(&inbox[0].2).0;
        assert_eq!(ack.verdict.as_deref(), Some("handled"), "{arm}");
        assert_eq!(Body::decode(&inbox[1].2).0.text, text, "{arm}");
        let (at, subject, raw, _) = &inbox[1];
        let line = crate::person::inbox_delivery_line(
            (FLEET, "cap"),
            *at,
            subject,
            raw,
            crate::now_ms(),
            None,
            &|_| None,
        );
        assert!(
            line.contains(&format!(
                " answer  from=s-q@{} trust=agent re={names}",
                mq.node
            )),
            "{arm}: the person's inbox reads it as s-q's on Q: {line}"
        );
        drop((from_p, to_p));
    }
}

/// **A PERSON'S ANSWER ENDS THE WAIT OF THE SESSION ON ANOTHER MAC THAT ASKED** (§11 C3d): s-q on
/// Q asks `@h-andrew@<P>` with a deadline. Q notes that the ask went to P and arms its deadline;
/// Q's push carries it to P's log, and P's bridge hands it to the person's lane on P, stamped with
/// the ask's offset on Q ([`crate::person::WIRE_RE_TOKEN`]). The person answers the copy by its
/// offset in their inbox, as `aterm fabric post` does ([`crate::person::wire_re`]): the sealed
/// answer names the ask by its offset on Q, P's carry and push bring it to Q, and Q's bridge hands
/// it to s-q as the person's answer to that ask, from where the ask went (P, whose lane brought
/// it), and settles the ask's deadline.
/// NEGATIVE CONTROLS: with [`UNSTAMPED_COPY`] on P, the copy carries no offset on Q, as before:
/// the answer names the copy's offset on P, and the ask's deadline on Q stands; with
/// [`PERSON_REPLY_AS_SENDER`] on Q, Q judges the answer by the sender it names,
/// `h-andrew@<P>`, which the ask did not go to: it is demoted to a note, and the deadline stands.
#[test]
fn a_persons_answer_ends_the_wait_of_the_session_on_another_mac_that_asked() {
    use crate::person::WIRE_RE_TOKEN;
    for arm in ["stamped", "unstamped", "as-sender"] {
        let stamped = arm != "unstamped";
        let (mp, mq) = (Mac::new(63), Mac::new(64));
        let both = [mp.node.as_str(), mq.node.as_str()];
        let (kp, kq) = (
            keys_for(63, Some(&members(&mp, &both))),
            keys_for(64, Some(&members(&mq, &both))),
        );
        assert!(crate::person_key::vouch(kp.path(), &[63; 32], crate::now_ms()).is_ok());
        // Q's log runs ahead of P's, so an offset on P names no record of the ask's on Q.
        let ev = subject::node_face(FLEET, &mq.node, "ev");
        let mut writer = mq.client(mq.key, &[format!("rw,p={}:{ev}", mq.node)]);
        for seq in 0..32 {
            writer
                .publish(pid(&mq.node), (1 << 20) + seq, &ev, b"v=1 t=1")
                .expect("an ev record");
        }
        let (lane_q, mut q, _served_q) = attached(&mq, "s-q", kq.path());
        let (_lane_p, mut p, _served_p) = attached(&mp, "s-p", kp.path());
        let net = Arc::new(Net::default());
        let from_p = pushes(&mp, &[&mq], &net, &members(&mp, &both));
        let to_p = pushes(&mq, &[&mp], &net, &members(&mq, &both));

        let text = "ship it?";
        *lane_q.outbox.lock().unwrap() = Some(format!(
            "post sid=s-q id=1 to=@h-andrew@{} kind=ask dl=600000 len={}\n{text}",
            mp.node,
            text.len()
        ));
        q.drain_outbox();
        let asked = format!("/f/{FLEET}/in/{}/h-andrew/{}/ask", mp.node, mq.node);
        let ask = mq.holds(&asked).first().expect("s-q's ask on Q's log").0;
        assert!(
            q.deadlines.contains_key(&ask),
            "{arm}: Q arms the ask's deadline"
        );
        until(&format!("{arm}: the ask reaches P's log"), || {
            !mp.holds(&asked).is_empty()
        });
        let at_p = mp.holds(&asked)[0].0;
        UNSTAMPED_COPY.with(|on| on.set(arm == "unstamped"));
        assert!(
            offer_live_record(&mut p, at_p),
            "{arm}: P's bridge reads it"
        );
        UNSTAMPED_COPY.with(|on| on.set(false));
        let handed = mp.holds(&format!("/f/{FLEET}/in/p/h-andrew/{}/>", mp.node));
        assert_eq!(handed.len(), 1, "{arm}: handed to the person once");
        let (copy, _, raw, _) = &handed[0];
        let body = Body::decode(raw).0;
        assert_eq!(
            body.from.as_deref(),
            Some(format!("s-q@{}", mq.node).as_str()),
            "{arm}"
        );
        assert_eq!(
            body.unknown.get(WIRE_RE_TOKEN).cloned(),
            stamped.then(|| crate::person::wire_stamp(ask, pid(&mq.node))),
            "{arm}: what the copy says the ask's offset on Q is, and whose push brought it"
        );
        assert_ne!(
            *copy, ask,
            "{arm}: the copy's offset on P is not the ask's on Q"
        );

        // The person answers the copy by its offset in their inbox, as `aterm fabric post` does.
        let handoff = members(&mp, &both);
        let mut person = mp.client(mp.key, &[format!("ro:/f/{FLEET}/in/p/h-andrew/>")]);
        let wire = crate::person::wire_re(
            &mut person,
            (FLEET, "h-andrew"),
            &handoff,
            ("s-q", &mq.node),
            *copy,
        )
        .expect("the person's lane reads");
        assert_eq!(
            wire,
            stamped.then_some(ask),
            "{arm}: the ask's offset on Q, which the answer names, where the copy says it"
        );
        assert_eq!(
            crate::person::wire_re(
                &mut person,
                (FLEET, "h-andrew"),
                &handoff,
                ("s-other", &mq.node),
                *copy,
            )
            .expect("reads"),
            None,
            "{arm}: the copy is no post of another session's, so it names nothing of one's"
        );
        assert_eq!(
            crate::person::wire_re(
                &mut person,
                (FLEET, "h-andrew"),
                &handoff,
                ("s-q", &mp.node),
                *copy,
            )
            .expect("reads"),
            None,
            "{arm}: the copy is from s-q on Q, so it names nothing of an s-q on P"
        );
        // Unstamped, `aterm fabric post` refuses the answer ([`crate::person::wire_re`]); posted
        // anyway, naming the copy's offset on P, as it was before §11 C3d, it names no ask on Q.
        let re = wire.unwrap_or(*copy);
        person_posts_re(
            &mp,
            kp.path(),
            (&mq.node, 2, "answer", "yes"),
            Some(re),
            Seal::Own,
        );
        let carried = format!("/f/{FLEET}/in/{}/s-q/{}/answer", mq.node, mp.node);
        until(
            &format!("{arm}: the person's answer reaches Q's log"),
            || !mq.holds(&carried).is_empty(),
        );
        let at_q = mq.holds(&carried)[0].0;
        PERSON_REPLY_AS_SENDER.with(|on| on.set(arm == "as-sender"));
        assert!(
            offer_live_record(&mut q, at_q),
            "{arm}: Q's bridge reads it"
        );
        PERSON_REPLY_AS_SENDER.with(|on| on.set(false));
        let deliveries = lane_q.deliveries();
        let line = deliveries
            .iter()
            .find(|line| line.starts_with("deliver s-q ") && line.ends_with(" text=yes"))
            .unwrap_or_else(|| panic!("{arm}: s-q was handed the answer: {deliveries:?}"));
        let kind = if arm == "as-sender" { "note" } else { "answer" };
        assert!(
            line.contains(&format!(
                " from=h-andrew@{} kind={kind} trust=human",
                mp.node
            )) && line.contains(&format!(" re={re}")),
            "{arm}: the person's answer, naming {re}: {line}"
        );
        assert_eq!(
            line.contains(" demoted=answer"),
            arm == "as-sender",
            "{arm}: whether it reads as a stray reply: {line}"
        );
        assert_eq!(
            q.deadlines.contains_key(&ask),
            arm != "stamped",
            "{arm}: whether the ask still waits"
        );
        if stamped {
            // Once retention gave the copy up, it names nothing: the read below the log's oldest
            // record, which the broker refuses, is read again told, and finds no post at `copy`.
            mp.broker
                .set_retention(crate::broker_rules::mesh_rules_scaled(1, 1, 1 << 30))
                .expect("P's rules");
            let ev = subject::node_face(FLEET, &mp.node, "ev");
            mp.client(mp.key, &[format!("rw,p={}:{ev}", mp.node)])
                .publish(pid(&mp.node), 1 << 21, &ev, b"v=1 t=1")
                .expect("an ev record");
            mp.broker
                .retention_tick_at(mp.broker.now_ms())
                .expect("a cut on P");
            let lanes = crate::subject::person_mail_filter(FLEET, "h-andrew");
            assert!(
                person
                    .fetch(*copy, &lanes, 1)
                    .is_err_and(|e| crate::transport::retained_past(&e, *copy).is_some()),
                "{arm}: the cut gave the copy up"
            );
            assert_eq!(
                crate::person::wire_re(
                    &mut person,
                    (FLEET, "h-andrew"),
                    &handoff,
                    ("s-q", &mq.node),
                    *copy,
                )
                .expect("a read below the oldest record is no failure"),
                None,
                "{arm}: a copy retention gave up names nothing"
            );
        }
        drop((from_p, to_p));
    }
}

/// **A COPY HELD BELOW THE PERSON'S LANES' OLDEST STILL NAMES THE ASK IT COPIES** (§11 C3d): a
/// cut carries the copy P's bridge handed the person below the base, and the rules set next take
/// the person's lanes whole from the base alone, so their oldest lies above the copy the broker
/// still holds. A person's answer to it reads it there as the bridge reads such a record, told
/// ([`crate::bridge::told_read_at`]), and names the ask by its offset on Q.
/// NEGATIVE CONTROL: [`crate::person::UNTOLD_WIRE_RE`], the refusal taken for no post: the copy
/// names nothing, and `aterm fabric post` would refuse the answer.
#[test]
fn a_copy_held_below_the_persons_lanes_oldest_still_names_the_ask_it_copies() {
    use crate::person::{wire_stamp, UNTOLD_WIRE_RE, WIRE_RE_TOKEN};
    for control in [false, true] {
        let (mp, mq) = (Mac::new(77), Mac::new(78));
        let both = [mp.node.as_str(), mq.node.as_str()];
        mp.broker
            .set_retention(super::lanes_whole_from_the_base(true))
            .expect("P's rules");
        let lane = format!("/f/{FLEET}/in/p/h-andrew/{}/ask", mp.node);
        let mut bridge = mp.client(mp.key, &[format!("rw,p={}:{lane}", mp.node)]);
        let mut hand = |seq: u64, text: &str| {
            let mut body = Body::new(0);
            body.from = Some(format!("s-q@{}", mq.node));
            body.text = text.to_owned();
            body.unknown
                .insert(WIRE_RE_TOKEN.to_owned(), wire_stamp(seq, pid(&mq.node)));
            bridge
                .publish(pid(&mp.node), seq, &lane, &body.encode())
                .expect("a copy for the person")
                .0
        };
        let copy = hand(7, "held");
        let later = hand(8, "after");
        mp.broker
            .retain_before_carrying_state(later + 1)
            .expect("a cut that carries the copies");
        mp.broker
            .set_retention(super::lanes_whole_from_the_base(false))
            .expect("P's rules");
        let lanes = subject::person_mail_filter(FLEET, "h-andrew");
        assert!(
            mp.broker
                .oldest(&astream_wire::Filter::new(lanes.clone()).expect("a filter"))
                > copy,
            "PRECONDITION: the person's lanes' oldest lies above the copy the broker holds"
        );
        let mut person = mp.client(mp.key, &[format!("ro:{lanes}")]);
        assert!(
            person
                .fetch(copy, &lanes, 1)
                .is_err_and(|e| crate::transport::retained_past(&e, copy).is_some()),
            "PRECONDITION: a plain read there is refused"
        );
        UNTOLD_WIRE_RE.with(|on| on.set(control));
        let wire = crate::person::wire_re(
            &mut person,
            (FLEET, "h-andrew"),
            &members(&mp, &both),
            ("s-q", &mq.node),
            copy,
        );
        UNTOLD_WIRE_RE.with(|on| on.set(false));
        assert_eq!(
            wire.expect("the person's lanes read"),
            (!control).then_some(7),
            "control={control}: the ask's offset on Q, read told below the oldest"
        );
    }
}

/// **A PERSON'S POST OUTLIVES A CARRY OUTAGE ACROSS A RETENTION CUT** (§11 C3d): a Mac's mesh
/// `bus.log` holds its mail lanes until the person carry has read them as well as the push
/// ([`crate::broker_rules::mesh_rules`]). P's carry carries a first post, then goes down while
/// P's push to Q runs on: a person's ask to `@s-q@<Q>`, then a note of P's own to Q that the push
/// carries, committing its group past both, lie below a retention cut. The cut keeps the ask;
/// once the carry is back it carries the ask to Q, and the next cut releases it on P.
/// NEGATIVE CONTROL: under [`crate::broker_rules::default_rules`], which hold mail for the push
/// alone, the cut gives the ask up: the carry, back, resumes past it, its journal says retention
/// gave posts up below it, and Q never gets the ask.
#[test]
fn a_persons_post_outlives_a_carry_outage_across_a_retention_cut() {
    use crate::broker_rules::{mesh_rules_scaled, rules_scaled};
    use crate::federate::{carry_group, mail_group, tests::routes_noting};
    use std::sync::atomic::Ordering::SeqCst;
    for arm in ["mesh", "default-rules"] {
        let (mp, mq) = (Mac::new(71), Mac::new(72));
        // The bulk stream holds the cut ([`astream_broker::Retention`]): at one byte, each `ev`
        // record P writes puts every record before it below the cut, where the mail rule carries
        // what it still wants and gives up the rest.
        let rules = if arm == "mesh" {
            mesh_rules_scaled(1, 1 << 30, 1 << 30)
        } else {
            rules_scaled(1, 1 << 30, 1 << 30)
        };
        mp.broker.set_retention(rules).expect("P's rules");
        let both = [mp.node.as_str(), mq.node.as_str()];
        let kp = keys_for(71, Some(&members(&mp, &both)));
        assert!(crate::person_key::vouch(kp.path(), &[71; 32], crate::now_ms()).is_ok());
        let handoff = members(&mp, &both);
        let net = Arc::new(Net::default());
        let ev = subject::node_face(FLEET, &mp.node, "ev");
        // Numbered above P's own note's sequence, which astream would answer as a duplicate.
        let mut seq = 1 << 20;
        let mut cut = || {
            seq += 1;
            mp.client(mp.key, &[format!("rw,p={}:{ev}", mp.node)])
                .publish(pid(&mp.node), seq, &ev, b"v=1 t=1")
                .expect("an ev record");
            mp.broker
                .retention_tick_at(mp.broker.now_ms())
                .expect("a cut on P");
        };
        let journal = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let start = || {
            let journal = Arc::clone(&journal);
            let routes = routes_noting(
                &mp,
                (&mp.node, mp.key),
                &BTreeSet::new(),
                (vec![Peer::of(&mq)], &net, false),
                Arc::new(move |line: &str| journal.lock().unwrap().push(line.to_owned())),
                Arc::new(|_| true),
            );
            routes.set(&handoff, &BTreeSet::new());
            routes
        };
        let routes = start();
        let first = person_posts(&mp, kp.path(), (&mq.node, 1, "note", "first"), Seal::Own);
        until(&format!("{arm}: the carry passed the first post"), || {
            mp.broker.group_start(&carry_group()) > first
        });
        drop(routes);

        mp.carry_down.store(true, SeqCst);
        let routes = start();
        let ask = person_posts(&mp, kp.path(), (&mq.node, 2, "ask", "kept?"), Seal::Own);
        let after = mp.post(&mq.node, 1, b"after");
        until(&format!("{arm}: the push passed P's own note"), || {
            mp.broker.group_start(&mail_group(&mq.node)) > after
        });
        assert_eq!(routes.carry_status().carried, 0, "{arm}: the carry is down");
        cut();
        let posts = format!("/f/{FLEET}/in/{}/s-q/h-andrew/ask", mq.node);
        let kept: Vec<u64> = mp.holds(&posts).iter().map(|row| row.0).collect();
        assert_eq!(
            kept,
            if arm == "mesh" { vec![ask] } else { Vec::new() },
            "{arm}: what the cut kept of the ask"
        );
        drop(routes);

        mp.carry_down.store(false, SeqCst);
        let routes = start();
        let carried = format!("/f/{FLEET}/in/{}/s-q/{}/ask", mq.node, mp.node);
        until(
            &format!("{arm}: the carry, back, passed P's own note"),
            || mp.broker.group_start(&carry_group()) > after,
        );
        if arm == "mesh" {
            until(&format!("{arm}: the ask reaches Q"), || {
                !mq.holds(&carried).is_empty()
            });
            let got = mq.holds(&carried);
            assert_eq!(got.len(), 1, "{arm}: once");
            assert_eq!(Body::decode(&got[0].2).0.text, "kept?", "{arm}");
            until(&format!("{arm}: the push passed the carried ask"), || {
                mp.broker.group_start(&mail_group(&mq.node)) > got[0].3.expect("stamped").1
            });
            cut();
            assert!(mp.holds(&posts).is_empty(), "{arm}: released once carried");
        } else {
            assert!(mq.holds(&carried).is_empty(), "{arm}: Q never gets the ask");
            let said = journal.lock().unwrap().clone();
            assert!(
                said.iter().any(
                    |line| line.starts_with("mesh: the person carry resumed at ")
                        && line.contains("retention gave up the posts below it")
                ),
                "{arm}: {said:?}"
            );
        }
        drop(routes);
    }
}

/// **A CARRIED POST REACHES ITS MAC THOUGH THE NODE SERVICE RESTARTS BEFORE ITS PUSH RAN** (§11
/// C3d): with Q cut off, P's carry carries a person's ask to `@s-q@<Q>` onto P's lane to Q, and
/// P's node service stops before its push to Q carried it. Started again, its watch reads the
/// lane's row and plans the push (a note of P's own to R, a push it starts after reading every
/// row, shows the read is done), and once Q is back the ask reaches Q's log, once.
/// NEGATIVE CONTROL: [`crate::federate::GIVE_BACK_CARRIED`], the carry before C3d giving each lane
/// back at once: the restarted watch never sees the lane, plans no push to Q, and Q gets nothing.
#[test]
fn a_carried_post_reaches_its_mac_though_the_node_service_restarts_before_its_push() {
    use crate::federate::GIVE_BACK_CARRIED;
    for control in [false, true] {
        let case = format!("control={control}");
        let (mp, mq, mr) = (Mac::new(81), Mac::new(82), Mac::new(83));
        let all = [mp.node.as_str(), mq.node.as_str(), mr.node.as_str()];
        let kp = keys_for(81, Some(&members(&mp, &all)));
        assert!(crate::person_key::vouch(kp.path(), &[81; 32], crate::now_ms()).is_ok());
        let net = Arc::new(Net::default());
        net.cut(&mq.node);
        let start = || {
            GIVE_BACK_CARRIED.with(|on| on.set(control));
            let routes = routes_of(&mp, vec![Peer::of(&mq), Peer::of(&mr)], &net, false);
            GIVE_BACK_CARRIED.with(|on| on.set(false));
            routes.set(&members(&mp, &all), &BTreeSet::new());
            routes
        };
        let routes = start();
        person_posts(
            &mp,
            kp.path(),
            (&mq.node, 1, "ask", "still there?"),
            Seal::Own,
        );
        until(&format!("{case}: the carry carried the ask"), || {
            routes.carry_status().carried == 1
        });
        drop(routes);

        let routes = start();
        mp.post(&mr.node, 1, b"to r");
        until(
            &format!("{case}: the restarted watch read every row"),
            || routes.written().contains(&mr.node),
        );
        assert_eq!(
            routes.written().contains(&mq.node),
            !control,
            "{case}: whether the watch plans the push to Q"
        );
        net.heal(&mq.node);
        routes.kick();
        let carried = format!("/f/{FLEET}/in/{}/s-q/{}/ask", mq.node, mp.node);
        if control {
            until(&format!("{case}: P's note reaches R"), || {
                !mr.mail_for(&mr.node).is_empty()
            });
            assert!(mq.holds(&carried).is_empty(), "{case}: Q gets nothing");
        } else {
            until(&format!("{case}: the ask reaches Q"), || {
                !mq.holds(&carried).is_empty()
            });
            assert_eq!(mq.holds(&carried).len(), 1, "{case}: once");
        }
        drop(routes);
    }
}

/// **A CARRIED LANE'S CHARGE GOES BACK ONCE RETENTION GAVE IT UP** (§11 C3d): the carry gives
/// no lane back itself (a watch that starts again plans its push from the lane's row), so the mesh
/// rule forgets ([`crate::broker_rules::mesh_rules`]). P's carry carries a person's ask to
/// `@s-q@<Q>` and P's push carries it to Q; once both groups passed it, a retention cut gives up
/// the person's post and the carried copy, and the same tick forgets both lanes: they leave P's
/// last-value index and the carry's subject budget counts none. A later ask to s-q opens the
/// carried lane again and reaches Q, once.
/// NEGATIVE CONTROL: [`crate::broker_rules::mesh_rules_scaled_unforgetting`], the mesh rules
/// without the forget: the cut gives the records up all the same, yet both lanes keep their rows
/// and the carry's budget still counts its lane.
#[test]
fn a_carried_lanes_charge_goes_back_once_retention_gave_it_up() {
    use crate::broker_rules::{mesh_rules_scaled, mesh_rules_scaled_unforgetting};
    use crate::federate::{carry_group, mail_group, CARRIER};
    for forget in [true, false] {
        let arm = if forget { "forget" } else { "unforgetting" };
        let (mp, mq) = (Mac::new(91), Mac::new(92));
        // At one byte, each `ev` record P writes puts every record before it below the cut, as in
        // [`a_persons_post_outlives_a_carry_outage_across_a_retention_cut`].
        let rules = if forget {
            mesh_rules_scaled(1, 1 << 30, 1 << 30)
        } else {
            mesh_rules_scaled_unforgetting(1, 1 << 30, 1 << 30)
        };
        mp.broker.set_retention(rules).expect("P's rules");
        let both = [mp.node.as_str(), mq.node.as_str()];
        let kp = keys_for(91, Some(&members(&mp, &both)));
        assert!(crate::person_key::vouch(kp.path(), &[91; 32], crate::now_ms()).is_ok());
        let net = Arc::new(Net::default());
        let routes = routes_of(&mp, vec![Peer::of(&mq)], &net, false);
        routes.set(&members(&mp, &both), &BTreeSet::new());
        let posts = format!("/f/{FLEET}/in/{}/s-q/h-andrew/ask", mq.node);
        let carried = format!("/f/{FLEET}/in/{}/s-q/{}/ask", mq.node, mp.node);
        let passed = |at: u64| {
            mp.broker.group_start(&carry_group()) > at
                && mp.broker.group_start(&mail_group(&mq.node)) > at
        };

        person_posts(&mp, kp.path(), (&mq.node, 1, "ask", "first"), Seal::Own);
        until(&format!("{arm}: the ask reaches Q"), || {
            !mq.holds(&carried).is_empty()
        });
        let at = mp.holds(&carried).first().expect("the carried copy on P").0;
        until(
            &format!("{arm}: both groups passed the carried copy"),
            || passed(at),
        );
        let ev = subject::node_face(FLEET, &mp.node, "ev");
        mp.client(mp.key, &[format!("rw,p={}:{ev}", mp.node)])
            .publish(pid(&mp.node), 1 << 20, &ev, b"v=1 t=1")
            .expect("an ev record");
        let tick = mp
            .broker
            .retention_tick_at(mp.broker.now_ms())
            .expect("a cut on P");
        assert!(
            mp.holds(&posts).is_empty() && mp.holds(&carried).is_empty(),
            "{arm}: the cut gave both records up"
        );
        let lanes = format!("/f/{FLEET}/in/{}/>", mq.node);
        let (rows, _) = mp
            .client(mp.key, &[format!("ro:{lanes}")])
            .last(&lanes, "", 64)
            .expect("P's last-value index");
        let rows: BTreeSet<&str> = rows
            .iter()
            .map(|(_, subject, _)| subject.as_str())
            .collect();
        let budget = || {
            mp.client(mp.key, &[format!("rw,p={CARRIER}:{carried}")])
                .subject_budget(astream_cap::producer_id_of(CARRIER))
                .expect("the carry's budget")
                .expect("a budget")
                .subjects
        };
        // Which tick forgot is not asked: the broker's own tick thread may cut, and forget with
        // it, before this one. Ticks take turns, each forgetting under its turn what its cut gave
        // up, so whichever ran had forgotten by the time this one returned.
        if forget {
            assert!(rows.is_empty(), "{arm}: both lanes forgotten: {rows:?}");
            assert_eq!(budget(), 0, "{arm}: the carry's lane given back");
        } else {
            assert_eq!(tick.forgotten, 0, "{arm}");
            assert_eq!(
                rows,
                BTreeSet::from([posts.as_str(), carried.as_str()]),
                "{arm}: both lanes keep their rows"
            );
            assert_eq!(budget(), 1, "{arm}: the carry's lane still charged");
            drop(routes);
            continue;
        }

        let again = person_posts(&mp, kp.path(), (&mq.node, 2, "ask", "again"), Seal::Own);
        until(&format!("{arm}: the later ask reaches Q"), || {
            mq.holds(&carried).len() == 2
        });
        let texts: Vec<String> = mq
            .holds(&carried)
            .iter()
            .map(|row| Body::decode(&row.2).0.text)
            .collect();
        assert_eq!(texts, ["first", "again"], "{arm}: each once");
        assert_eq!(
            mp.holds(&carried).first().and_then(|row| row.3),
            Some((astream_cap::producer_id_of(CARRIER), again)),
            "{arm}: the lane opened again under the carrier"
        );
        assert_eq!(budget(), 1, "{arm}: charged again");
        drop(routes);
    }
}

/// One run of `FabricMoves`' world: A's `bus.log` under three nodes of A's key in turn, B's ask to
/// `s-a` on it for the first, each node's bridge on the one bridge state dir with its own aterm
/// endpoint lane (one aterm throughout: [`shown_to`] reads their lines in order), and the node
/// service's moves run by [`crate::federate::tests::move_once`].
struct Moves {
    nodes: [Mac; 3],
    state: std::path::PathBuf,
    ask: BrokerRecord,
    lanes: Vec<FakeVerbLane>,
    bridge: Option<Bridge>,
    keys: Vec<aterm_tempfile::TempDir>,
    serving: Vec<astream_broker::BrokerHandle>,
    sender: String,
    endpoint: u32,
    r: usize,
}

impl Moves {
    fn new(control: bool) -> Self {
        use aterm_types::control_verbs::{BRIDGE_LANE, LINK_MOVED_LANE};
        let n0 = Mac::new(101);
        let nodes = [n0.rotated(102), n0.rotated(103)];
        let nodes = {
            let [n1, n2] = nodes;
            [n0, n1, n2]
        };
        let state = nodes[0].link_state();
        let mut run = Self {
            nodes,
            state,
            ask: (0, String::new(), Vec::new()),
            lanes: Vec::new(),
            bridge: None,
            keys: Vec::new(),
            serving: Vec::new(),
            sender: String::new(),
            endpoint: if control {
                LINK_MOVED_LANE - 1
            } else {
                BRIDGE_LANE
            },
            r: 0,
        };
        run.start_bridge(0);
        // B's ask lands once N0's bridge is attached: its tail takes it, as `Deliver0` hands it.
        let mb = Mac::new(104);
        let subject = run.lane(0);
        let mut body = Body::new(0);
        body.from = Some("s-b".to_owned());
        body.text = "moved".to_owned();
        let raw = body.encode();
        let off = run.nodes[0]
            .client(
                mb.key,
                &[format!(
                    "rw,p={0}:/f/{FLEET}/in/{1}/*/{0}/*",
                    mb.node, run.nodes[0].node
                )],
            )
            .publish(
                pid(&mb.node),
                7,
                &format!("{subject}/{}/ask", mb.node),
                &raw,
            )
            .expect("B's push lands")
            .0;
        run.ask = (off, format!("{subject}/{}/ask", mb.node), raw);
        run.sender = mb.node;
        run
    }

    /// `s-a`'s lanes for node `i`, B's node left off.
    fn lane(&self, i: usize) -> String {
        format!("/f/{FLEET}/in/{}/s-a", self.nodes[i].node)
    }

    /// The copies node `i`'s lanes hold for `s-a`.
    fn held(&self, i: usize) -> Vec<crate::federate::tests::HeldRecord> {
        self.nodes[0].holds(&format!("{}/>", self.lane(i)))
    }

    /// The hand-off under node `i`: every earlier node moved to it.
    fn handoff(&self, i: usize) -> Handoff {
        Handoff {
            me: self.nodes[i].node.clone(),
            members: [self.nodes[i].node.clone()].into(),
            moved: self.nodes[..i]
                .iter()
                .map(|old| (old.node.clone(), self.nodes[i].node.clone()))
                .collect(),
            rank: BTreeMap::new(),
            gone: BTreeSet::new(),
        }
    }

    /// Node `i`'s bridge, attached before any copy lands on its lanes.
    fn start_bridge(&mut self, i: usize) {
        let handoff = self.handoff(i);
        let keys = keys_for(101 + i as u8, (i > 0).then_some(&handoff));
        if i > 0 {
            crate::federate::write_old_reads(keys.path(), &handoff, &|grant| {
                Mac::mint(&self.nodes[0].secret, grant)
            })
            .expect("the node service's reader's ring");
        }
        let (lane, mut bridge, served) = bridge_hosting(
            &self.nodes[i],
            &[(1, "s-a")],
            keys.path(),
            Some(&self.state),
            crate::fx::Fx::os(),
        );
        bridge.endpoint_lane = self.endpoint;
        assert!(bridge.attach_broker(), "node {i}'s bridge attaches");
        self.lanes.push(lane);
        self.bridge = Some(bridge);
        self.keys.push(keys);
        self.serving.push(served);
    }

    /// The running bridge delivers `rec`, its commit held below it.
    fn deliver(&mut self, rec: crate::federate::tests::HeldRecord) {
        let (off, subject, body, origin) = rec;
        let bridge = self.bridge.as_mut().expect("a bridge runs");
        bridge.commit_floor = Some(off);
        bridge.in_round = true;
        bridge.on_inbox_record(&(off, subject, body), origin);
        bridge.in_round = false;
    }

    /// The copy node `i` holds of the record numbered `seq` on the node it moved from.
    fn copy_of(&self, i: usize, seq: u64) -> Option<crate::federate::tests::HeldRecord> {
        self.held(i)
            .into_iter()
            .find(|(.., origin)| origin.is_some_and(|(_, s)| s == seq))
    }

    fn rotate(&mut self) {
        let mut old = self.bridge.take().expect("a bridge runs");
        old.conn = None;
        old.wind_down();
        drop(old);
        self.r += 1;
        self.start_bridge(self.r);
    }

    /// The node service's move of node `old`'s unread mail to the node A's key is now, run whole.
    fn moved(&self, old: usize) {
        crate::federate::tests::move_once(
            &self.nodes[0],
            &self.nodes[self.r],
            &self.nodes[old].node,
            &self.state,
        )
        .expect("the move runs");
    }

    /// What a burst of the move to N1 writes, its acknowledgment lost: the ask under N1's producer,
    /// numbered by its offset on N0's lane, on N1's. `MoveAck1`, the move run whole, lands nothing
    /// more: the broker's dedup holds the two for one.
    fn burst(&self) {
        let (off, _, body) = &self.ask;
        let n1 = &self.nodes[1];
        self.nodes[0]
            .client(n1.key, &[format!("rw,p={0}:/f/{FLEET}/in/{0}/>", n1.node)])
            .publish(
                pid(&n1.node),
                *off,
                &format!("{}/{}/ask", self.lane(1), self.sender),
                body,
            )
            .expect("the burst lands");
    }

    fn apply(&mut self, action: &str) {
        let x = self.ask.0;
        match action {
            "Rotate1" | "Rotate2" => self.rotate(),
            "Deliver0" => {
                let rec = self.held(0).into_iter().next().expect("the ask");
                self.deliver(rec);
            }
            "MoveLand1" => self.burst(),
            "MoveAck1" => self.moved(0),
            "Deliver1" => {
                let rec = self.held(1).into_iter().next().expect("the first copy");
                self.deliver(rec);
            }
            "MoveLand2a" => self.moved(0),
            "MoveLand2b" => self.moved(1),
            "DeliverA" => {
                let rec = self.copy_of(2, x).expect("the move of N0's mail's copy");
                self.deliver(rec);
            }
            "DeliverB" => {
                let y = self.held(1)[0].0;
                let rec = self.copy_of(2, y).expect("the move of N1's mail's copy");
                self.deliver(rec);
            }
            other => panic!("FabricMoves has no action {other} the code takes"),
        }
    }

    /// The model's variables, read off A's `bus.log` and what aterm was sent.
    fn project(&self) -> aterm_spec::interp::State {
        let x = self.ask.0;
        let y = self.held(1).first().map(|rec| rec.0);
        let lines: Vec<String> = self.lanes.iter().flat_map(taken).collect();
        let names = |off: u64| {
            lines.iter().any(|line| {
                line.split_whitespace()
                    .any(|tok| tok == format!("off={off}") || tok == format!("was={off}"))
            })
        };
        let took = |i: usize, off: Option<u64>| {
            off.is_some_and(|off| {
                self.lanes
                    .get(i)
                    .is_some_and(|lane| lane.delivery_offsets().contains(&off))
            })
        };
        let za = self.copy_of(2, x).map(|rec| rec.0);
        let zb = y.and_then(|y| self.copy_of(2, y)).map(|rec| rec.0);
        let flag = |b: bool| i64::from(b);
        [
            ("r", self.r as i64),
            ("d0", flag(took(0, Some(x)))),
            ("y", self.held(1).len() as i64),
            (
                "c0",
                flag(
                    self.nodes[0]
                        .broker
                        .group_start(&crate::federate::mail_group(&self.nodes[0].node))
                        > x,
                ),
            ),
            ("d1", flag(took(1, y))),
            ("za", flag(za.is_some())),
            ("zb", flag(zb.is_some())),
            ("da", flag(took(2, za))),
            ("db", flag(took(2, zb))),
            ("kx", flag(names(x))),
            ("ky", flag(y.is_some_and(names))),
            ("shown", shown_to(&lines, "s-a").len() as i64),
            ("lost", 0),
        ]
        .into_iter()
        .collect()
    }

    /// A fresh world run down `path`, with the state projected after each step.
    fn replayed(path: &[&str], control: bool) -> Vec<aterm_spec::interp::State> {
        let mut run = Self::new(control);
        path.iter()
            .map(|action| {
                run.apply(action);
                run.project()
            })
            .collect()
    }
}

/// **THE OWN-INBOX MOVE REFINES `FabricMoves`** (§11 C3e, C7's "mail delivered once per destination
/// session"): in every state the model reaches, every action it enables takes A's `bus.log` and
/// what aterm shows where the model goes. Across two rotations, a move's unacknowledged burst
/// moved again, and every bridge's commit held below what it delivered, `s-a` is shown B's ask
/// once, and shown it once some node's bridge runs.
/// NEGATIVE CONTROL: an endpoint before [`aterm_types::control_verbs::LINK_MOVED_LANE`] (mutant
/// 1): the run leaves the model at `Deliver1`, the first copy shown again. (Mutant 2, a copy
/// followed back one move alone, is the model's: `was=` names the first offset through both
/// moves in `mail_unread_across_two_rotations_reaches_its_session_once`.)
#[test]
fn the_own_inbox_move_conforms_to_the_fabric_moves_model() {
    use crate::federate::conformance::{checked, diverges, paths, successor};
    let model = aterm_models_swarm::fabric_moves_model();
    checked(
        &model,
        &aterm_models_swarm::mail_shown_liveness(),
        &[(1, "ShownOnce"), (1, "AckOnlyCarried"), (2, "ShownOnce")],
    );
    let order = paths(&model);
    assert_eq!(
        order.len(),
        aterm_spec::interp::reachable_states(&model).len(),
        "the walk reaches every state"
    );
    let jobs: Vec<(&aterm_spec::interp::State, &[&'static str], &'static str)> = order
        .iter()
        .flat_map(|(state, path)| {
            model
                .actions
                .iter()
                .filter(|action| model.action_enabled(action.name, state))
                .map(move |action| (state, path.as_slice(), action.name))
        })
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = aterm_cores::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(8);
    let judged: usize = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut judged = 0;
                    while let Some((state, path, action)) =
                        jobs.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                    {
                        let mut steps = path.to_vec();
                        steps.push(action);
                        let seen = Moves::replayed(&steps, false);
                        assert_eq!(
                            seen.last(),
                            Some(&successor(&model, action, state)),
                            "FabricMoves: {action} from {state:?} (down {path:?})"
                        );
                        judged += 1;
                    }
                    judged
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    });
    eprintln!(
        "FabricMoves: {} states, {judged} transitions judged",
        order.len()
    );

    let path = ["Deliver0", "Rotate1", "MoveLand1", "Deliver1"];
    let seen = Moves::replayed(&path, true);
    assert_eq!(diverges(&model, &path, &seen), "Deliver1");
    assert_eq!(
        seen.last().unwrap()["shown"],
        2,
        "the first copy shown again"
    );
    assert!(!model.check_invariant("ShownOnce", seen.last().unwrap()));
}
