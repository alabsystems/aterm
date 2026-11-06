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
    let dir = aterm_tempfile::tempdir().expect("scratch");
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
    let mesh = MeshPaths {
        keys: keys.to_path_buf(),
        root: keys.to_path_buf(),
    };
    let (lane, mut bridge) = FakeVerbLane::bridge_configured(&[(1, sid)], state, Some(mesh));
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
