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
        until(
            &format!("{case}: the carry took the post to no member"),
            || {
                new.holds(&notices)
                    .iter()
                    .any(|(_, _, body, _)| Body::decode(body).0.re == Some(unmet))
            },
        );
        let status = routes.carry_status();
        assert_eq!(
            (status.carried, status.refused),
            (0, if control { 2 } else { 1 }),
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
        assert!(
            ma.broker.group_start(&mail_group(&ma.node)) > late,
            "{case}: the old group passes the late ask"
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

/// **MAIL UNREAD ACROSS TWO ROTATIONS OF A MAC'S KEY REACHES ITS SESSION ONCE** (§11 C3b): B's ask
/// to `s-a` lands on A's `bus.log` for A's node, and A's key rotates twice before `s-a` reads it.
/// The node service under the second new node moves the first move's copy on, and A's bridge
/// delivers the ask once; its receipt follows the copy back through both moves, under the node
/// service's reader's ring, to the ask as B's push wrote it, and names it by its offset on B.
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
        let (lane, mut a, _served) = attached(&last, "s-a", keys.path());
        let copy = held(&last.node)[0];
        rounds_until(&format!("{case}: the bridge delivers"), &mut a, || {
            lane.delivery_offsets().contains(&copy)
        });
        assert_eq!(lane.delivery_offsets(), [copy], "{case}: once");
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
    (dst, seq, kind, text): (&str, u64, &str, &str),
    seal: Seal<'_>,
) -> u64 {
    use crate::person_key::{post_address, read_cert, PersonKey};
    let person = "h-andrew";
    let lane = subject::post_lane(FLEET, dst, "s-q", person, kind);
    let mut body = Body::new(crate::now_ms());
    body.text = text.to_owned();
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
/// person, and gives that lane's charge back; P's push carries it to Q's log once; and Q's
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
        assert!(
            charged.is_empty(),
            "{arm}: the carry gave back each lane it carried on: {:?}",
            charged
                .iter()
                .map(|(_, subject, _)| subject)
                .collect::<Vec<_>>()
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
