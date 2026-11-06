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
