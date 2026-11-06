// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Fleet-wide halts over the mesh (§11 C4): Macs each running a bridge on their own guarded
// `bus.log`, as the node service hosts it, under their node's ring, with each Mac's halt relays
// (`crate::federate::Routes`, `Lane::Halts`) to every other member, of which it reaches the ones
// a test links it to. A person asks on their own Mac's log; its bridge says the halt on its
// node's halt subject, origin-signed, and every Mac's bridge applies what reaches its log. Every
// wait is the one-minute hang detector.

use super::*;
use crate::halt::{carried_id, Ask, Said, Word};

/// One Mac of a halt world: its log, its node key and hand-off, and its bridge hosting `sid`.
/// Fields drop in order: the bridge, its lane, the log's sockets, then the Mac. Every socket the
/// log was served on stays served until then, as the node service keeps it while a bridge
/// restarts: a dropped handle closes the broker.
struct Seat {
    bridge: Bridge,
    lane: FakeVerbLane,
    served: Vec<astream_broker::BrokerHandle>,
    sid: &'static str,
    state: aterm_tempfile::TempDir,
    keys: aterm_tempfile::TempDir,
    mac: Mac,
}

impl Seat {
    fn held(&self) -> bool {
        self.lane.holds(self.sid)
    }

    /// This Mac's bridge started again, attached, on its state dir, keys and log, once the one
    /// before it stopped (a bridge running on the state dir would keep the next from it).
    fn restarted(self) -> Self {
        let Self {
            mut served,
            mut bridge,
            lane,
            sid,
            state,
            keys,
            mac,
        } = self;
        bridge.close_subscriptions();
        drop((bridge, lane));
        let (lane, mut bridge, again) = bridge_on(
            &mac,
            sid,
            keys.path(),
            Some(state.path()),
            crate::fx::Fx::os(),
        );
        served.push(again);
        // A bridge on a state dir an earlier one left reads the outbox it may have left owed.
        *lane.outbox.lock().unwrap() = Some(String::new());
        assert!(bridge.attach_broker(), "the bridge attaches again");
        Self {
            bridge,
            lane,
            served,
            sid,
            state,
            keys,
            mac,
        }
    }

    /// This Mac after a `Node` record rotated its key to `tag`'s: its node key replaced, and its
    /// bridge started again under the node that key names.
    fn rekeyed(mut self, tag: u8) -> Self {
        let seed = format!("{}\n", aterm_codec::hex::encode_lower(&[tag; 32]));
        aterm_uds::private_file::write_private(
            self.keys.path(),
            crate::federate::NODE_KEY_FILE,
            seed.as_bytes(),
        )
        .expect("the new node key");
        self.mac = self.mac.as_key(tag);
        self.restarted()
    }
}

/// A halt id, distinct for each `n`.
fn id(n: u64) -> String {
    format!("{n:032x}")
}

/// Macs `tags`, every one a member, each broker taking every member's relayed records, each
/// bridge attached and hosting the session `sids` names for it.
fn world(tags: &[u8], sids: &[&'static str]) -> Vec<Seat> {
    let macs: Vec<Mac> = tags.iter().map(|tag| Mac::new(*tag)).collect();
    let nodes: Vec<&str> = macs.iter().map(|mac| mac.node.as_str()).collect();
    let handoffs: Vec<Handoff> = macs.iter().map(|mac| members(mac, &nodes)).collect();
    let origins: Vec<[u8; 32]> = macs.iter().map(|mac| mac.key).collect();
    macs.into_iter()
        .zip(handoffs)
        .zip(tags.iter().zip(sids))
        .map(|((mac, handoff), (tag, sid))| {
            mac.broker.set_origin_keys(origins.iter().copied());
            let keys = keys_for(*tag, Some(&handoff));
            let state = aterm_tempfile::tempdir().expect("a state dir");
            let (lane, mut bridge, served) = bridge_on(
                &mac,
                sid,
                keys.path(),
                Some(state.path()),
                crate::fx::Fx::os(),
            );
            assert!(bridge.attach_broker(), "the bridge attaches to its bus.log");
            Seat {
                bridge,
                lane,
                served: vec![served],
                sid,
                state,
                keys,
                mac,
            }
        })
        .collect()
}

/// The halt relays of `seats`: each Mac relays to every other, and reaches only the others
/// `links` pairs it with.
fn relays(seats: &[Seat], net: &Arc<Net>, links: &[(usize, usize)]) -> Vec<Routes> {
    let nodes: Vec<&str> = seats.iter().map(|seat| seat.mac.node.as_str()).collect();
    seats
        .iter()
        .enumerate()
        .map(|(i, seat)| {
            let peers: Vec<&Seat> = links
                .iter()
                .filter_map(|&(a, b)| match (a == i, b == i) {
                    (true, _) => Some(&seats[b]),
                    (_, true) => Some(&seats[a]),
                    _ => None,
                })
                .collect();
            let routes = routes_of(
                &seat.mac,
                peers.iter().map(|peer| Peer::of(&peer.mac)).collect(),
                net,
                false,
            );
            routes.set(&members(&seat.mac, &nodes), &BTreeSet::new());
            routes
        })
        .collect()
}

/// Run every bridge's rounds until `done`, which must happen.
fn settle(seats: &mut [Seat], what: &str, done: impl Fn(&[Seat]) -> bool) {
    until(what, || {
        for seat in seats.iter_mut() {
            run_rounds(&mut seat.bridge);
        }
        done(seats)
    });
}

/// `person`'s request on `mac`'s log, as `aterm fabric halt` writes it there under their ring.
fn ask(mac: &Mac, person: &str, seq: u64, ask: &Ask) -> io::Result<u64> {
    let mut client = mac.client(mac.key, &crate::enable::person_rules(FLEET, person));
    client
        .publish(
            pid(person),
            seq,
            &subject::halt_ask(FLEET, person),
            ask.render(seq).as_bytes(),
        )
        .map(|answer| answer.0)
}

fn on(n: u64) -> Ask {
    Ask {
        id: id(n),
        word: Word::On("stop".to_owned()),
        here: false,
    }
}

fn off(n: u64) -> Ask {
    Ask {
        id: id(n),
        word: Word::Off,
        here: false,
    }
}

/// What node `tag`'s bridge would say on its halt subject, written straight onto `mac`'s log
/// under that node's producer and signed with its key: a record another Mac's bridge said, as
/// its relay lands it.
fn said_by_node(mac: &Mac, tag: u8, seq: u64, said: &Said) {
    let node = node_of(&key_of(tag));
    let subject = subject::halt_subject(FLEET, &node);
    let body = astream_broker::origin::seal(&[tag; 32], &subject, seq, said.render().as_bytes());
    let mut client = mac.client(key_of(tag), &[format!("rw,p={node}:{subject}")]);
    client
        .publish(pid(&node), seq, &subject, &body)
        .expect("the node's record lands");
}

/// **AN AGENT CANNOT HALT**: an agent's ring (`a-`) writes no halt request, its own or a
/// person's, and a halt record that names an agent as its asker holds nothing on the Mac it
/// reaches, though it is its origin's, signed by its key.
/// NEGATIVE CONTROL: [`crate::halt::ANY_PRINCIPAL`] reads a record asked by any principal as a
/// person's: the agent's halt holds the session.
#[test]
fn an_agent_cannot_halt() {
    for control in [false, true] {
        crate::halt::ANY_PRINCIPAL.with(|c| c.set(control));
        let case = format!("control={control}");
        let x = node_of(&key_of(92));
        let mut seats = world(&[91], &["s-a"]);
        let a = &seats[0];
        let nodes = [a.mac.node.as_str(), x.as_str()];
        members(&a.mac, &nodes)
            .write(a.keys.path())
            .expect("X is a member");
        for person in ["a-worker", "h-alice"] {
            let written = a
                .mac
                .client(a.mac.key, &crate::enable::person_rules(FLEET, "a-worker"))
                .publish(
                    pid("a-worker"),
                    1,
                    &subject::halt_ask(FLEET, person),
                    on(1).render(1).as_bytes(),
                );
            assert!(
                written.is_err(),
                "{case}: an agent's ring writes {person}'s request"
            );
        }
        let forged = Said {
            id: id(2),
            by: "a-worker".to_owned(),
            word: Word::On("stop".to_owned()),
            t_ms: 1,
        };
        said_by_node(&seats[0].mac, 92, 1, &forged);
        // A record X said after it, which reaches the bridge after it on the one halt face.
        let mark = Said {
            id: id(3),
            by: "h-alice".to_owned(),
            word: Word::Off,
            t_ms: 2,
        };
        said_by_node(&seats[0].mac, 92, 2, &mark);
        settle(
            &mut seats,
            &format!("{case}: the bridge read X's records"),
            |s| s[0].bridge.mesh_halts.is_lifted(&id(3)),
        );
        assert_eq!(seats[0].held(), control, "{case}: the agent's halt holds");
        assert_eq!(seats[0].bridge.mesh_halts.knows(&id(2)), control, "{case}");
    }
    crate::halt::ANY_PRINCIPAL.with(|c| c.set(false));
}

/// **A REPLAYED HALT-OFF DOES NOT LIFT A LATER HALT**: a person halts, lifts that halt, and
/// halts again; the record of the halt-off comes again (as a neighbour's late resend delivers
/// it) and the bridge restarts and reads every request and halt record its log keeps: the later
/// halt holds throughout, for the halt-off names the id it lifted.
/// NEGATIVE CONTROL: [`crate::halt::OFF_LIFTS_EVERY`] (a halt-off lifts whatever stands, as the
/// old per-person row did), set once the later halt holds, so the bridge's own reading of its
/// halt-off cannot come first: the replayed halt-off releases the session.
#[test]
fn a_replayed_halt_off_does_not_lift_a_later_halt() {
    for control in [false, true] {
        let case = format!("control={control}");
        let mut seats = world(&[81], &["s-a"]);
        let mac = &seats[0].mac;
        ask(mac, "h-a", 1, &on(1)).expect("the halt");
        settle(&mut seats, &format!("{case}: the halt holds"), |s| {
            s[0].held()
        });
        ask(&seats[0].mac, "h-a", 2, &off(1)).expect("the halt-off");
        settle(&mut seats, &format!("{case}: the halt-off releases"), |s| {
            !s[0].held()
        });
        ask(&seats[0].mac, "h-a", 3, &on(2)).expect("the later halt");
        settle(&mut seats, &format!("{case}: the later halt holds"), |s| {
            s[0].held() && s[0].bridge.mesh_halts.knows(&id(2))
        });
        let a = &seats[0];
        let lifted = a
            .mac
            .holds(&subject::halt_subject(FLEET, &a.mac.node))
            .into_iter()
            .find(|(_, _, body, _)| {
                astream_broker::origin::payload(body)
                    .and_then(|payload| Said::parse(payload).ok())
                    .is_some_and(|said| said.id == id(1) && said.word == Word::Off)
            })
            .expect("the bridge said the halt-off on its halt subject");
        crate::halt::OFF_LIFTS_EVERY.with(|c| c.set(control));
        seats[0]
            .bridge
            .on_halt_record(&(lifted.0, lifted.1, lifted.2));
        assert_eq!(seats[0].held(), !control, "{case}: the resent halt-off");
        let a = seats.pop().expect("A");
        seats.push(a.restarted());
        assert_eq!(seats[0].held(), !control, "{case}: after a restart");
        assert_eq!(
            seats[0].bridge.mesh_halts.standing().contains_key(&id(2)),
            !control,
            "{case}"
        );
    }
    crate::halt::OFF_LIFTS_EVERY.with(|c| c.set(false));
}

/// **A HALT AND ITS HALT-OFF REACH EVERY MAC**: on a line of Macs A — B — C (A and C not
/// linked), a person's halt on A holds every Mac's session, C's by B's relay of A's record;
/// a person's halt-off on C lifts it on every Mac, A's by B's relay of C's.
/// NEGATIVE CONTROL: [`crate::federate::OWN_HALTS_ONLY`] (each Mac relays only what it said
/// itself): A's halt stops at B, and C holds a later halt B said but never A's.
#[test]
fn a_halt_off_lifts_on_every_mac() {
    for control in [false, true] {
        crate::federate::OWN_HALTS_ONLY.with(|c| c.set(control));
        let case = format!("control={control}");
        let mut seats = world(&[61, 62, 63], &["s-a", "s-b", "s-c"]);
        let net = Arc::new(Net::default());
        let routes = relays(&seats, &net, &[(0, 1), (1, 2)]);
        ask(&seats[0].mac, "h-a", 1, &on(1)).expect("the halt");
        if control {
            settle(&mut seats, &format!("{case}: B holds A's halt"), |s| {
                s[1].held() && s[1].bridge.mesh_halts.knows(&id(1))
            });
            ask(&seats[1].mac, "h-b", 1, &on(2)).expect("B's halt");
            settle(&mut seats, &format!("{case}: C holds B's halt"), |s| {
                s[2].bridge.mesh_halts.knows(&id(2))
            });
            assert!(
                !seats[2].bridge.mesh_halts.knows(&id(1)),
                "{case}: A's halt stopped at B"
            );
        } else {
            settle(&mut seats, &format!("{case}: every Mac holds"), |s| {
                s.iter().all(Seat::held)
            });
            ask(&seats[2].mac, "h-c", 1, &off(1)).expect("the halt-off");
            settle(&mut seats, &format!("{case}: every Mac lifts it"), |s| {
                s.iter()
                    .all(|seat| !seat.held() && seat.bridge.mesh_halts.is_lifted(&id(1)))
            });
        }
        drop(routes);
    }
    crate::federate::OWN_HALTS_ONLY.with(|c| c.set(false));
}

/// **ANY CURRENT MEMBER'S PERSON LIFTS A HALT**: a halt a person set on A is lifted on A and B by
/// a halt-off another person asked on B.
/// NEGATIVE CONTROL: [`crate::halt::OFF_FROM_ORIGIN_ONLY`] (a halt-off lifts only from the halt's
/// own Mac and person): B's halt-off lifts nothing; A, past a later record B said, still holds.
#[test]
fn a_halt_off_from_another_members_person_lifts_it() {
    for control in [false, true] {
        crate::halt::OFF_FROM_ORIGIN_ONLY.with(|c| c.set(control));
        let case = format!("control={control}");
        let mut seats = world(&[71, 72], &["s-a", "s-b"]);
        let net = Arc::new(Net::default());
        let routes = relays(&seats, &net, &[(0, 1)]);
        ask(&seats[0].mac, "h-a", 1, &on(1)).expect("the halt");
        settle(&mut seats, &format!("{case}: both Macs hold"), |s| {
            s.iter().all(Seat::held)
        });
        ask(&seats[1].mac, "h-b", 1, &off(1)).expect("the halt-off");
        if control {
            ask(&seats[1].mac, "h-b", 2, &off(9)).expect("a later record");
            settle(
                &mut seats,
                &format!("{case}: A read B's later record"),
                |s| s[0].bridge.mesh_halts.is_lifted(&id(9)),
            );
            assert!(seats.iter().all(Seat::held), "{case}: the halt stands");
        } else {
            settle(&mut seats, &format!("{case}: both Macs lift it"), |s| {
                s.iter()
                    .all(|seat| !seat.held() && seat.bridge.mesh_halts.is_lifted(&id(1)))
            });
        }
        drop(routes);
    }
    crate::halt::OFF_FROM_ORIGIN_ONLY.with(|c| c.set(false));
}

/// **NO HALT OUTLIVES ITS ORIGIN**: a halt a person set on A holds A, B and C. The membership log
/// then removes A, or a `Node` record rotates A's key to A2; the node service on each Mac names A
/// gone in its hand-off, and the halt lifts on every Mac: B and C, A itself when removed, and A
/// as A2 when its bridge restarts under its new key. A record A said that comes again applies to
/// nothing elsewhere; on A, removed, its person's later halt stops A, through a restart.
/// NEGATIVE CONTROL: [`crate::halt::NO_LIFT_ON_GONE`] (a gone origin's halts stand): B and C still
/// hold.
#[test]
fn removing_or_rotating_the_origin_lifts_its_halts_on_every_mac() {
    for (rotate, control) in [(false, false), (true, false), (false, true), (true, true)] {
        crate::halt::NO_LIFT_ON_GONE.with(|c| c.set(control));
        let case = format!("rotate={rotate} control={control}");
        let mut seats = world(&[51, 52, 53], &["s-a", "s-b", "s-c"]);
        let net = Arc::new(Net::default());
        let routes = relays(&seats, &net, &[(0, 1), (1, 2), (0, 2)]);
        ask(&seats[0].mac, "h-a", 1, &on(1)).expect("the halt");
        settle(&mut seats, &format!("{case}: every Mac holds"), |s| {
            s.iter().all(Seat::held)
        });
        drop(routes);
        let a = seats[0].mac.node.clone();
        let a2 = node_of(&key_of(54));
        let mut now: Vec<String> = seats[1..].iter().map(|s| s.mac.node.clone()).collect();
        if rotate {
            now.push(a2.clone());
        }
        let keys: Vec<[u8; 32]> = seats[1..]
            .iter()
            .map(|s| s.mac.key)
            .chain(rotate.then(|| key_of(54)))
            .collect();
        for seat in &mut seats {
            let me = if seat.mac.node == a && rotate {
                a2.clone()
            } else {
                seat.mac.node.clone()
            };
            let handoff = Handoff {
                me,
                members: now.iter().cloned().collect(),
                moved: if rotate {
                    [(a.clone(), a2.clone())].into()
                } else {
                    BTreeMap::new()
                },
                gone: [a.clone()].into(),
            };
            handoff
                .write(seat.keys.path())
                .expect("the node service's hand-off");
            seat.mac.broker.set_origin_keys(keys.iter().copied());
            seat.bridge.roster_backstop();
        }
        for seat in &seats[1..] {
            assert_eq!(seat.held(), control, "{case}: {} after A went", seat.sid);
        }
        if rotate {
            // A's bridge says nothing under a node its key no longer names; as A2 it lifts.
            let a = seats.remove(0);
            seats.insert(0, a.rekeyed(54));
            assert_eq!(seats[0].bridge.node, a2, "{case}: A runs as A2");
        }
        assert_eq!(seats[0].held(), control, "{case}: A itself");
        if !rotate && !control {
            // Out of the fleet, A's person still stops A, through a restart.
            ask(&seats[0].mac, "h-a", 2, &on(5)).expect("a halt after the removal");
            settle(&mut seats, &format!("{case}: A's person halts A"), |s| {
                s[0].held()
            });
            let a = seats.remove(0);
            seats.insert(0, a.restarted());
            assert!(
                seats[0].held(),
                "{case}: A's own halt holds after a restart"
            );
        }
        // What A said comes again, as a late relay delivers it: it applies to nothing.
        let again = Said {
            id: id(2),
            by: "h-a".to_owned(),
            word: Word::On("stop".to_owned()),
            t_ms: 3,
        };
        let seed = [51; 32];
        let subject = subject::halt_subject(FLEET, &a);
        let body = astream_broker::origin::seal(&seed, &subject, 1, again.render().as_bytes());
        seats[1].bridge.on_halt_record(&(1, subject, body));
        assert_eq!(
            seats[1].bridge.mesh_halts.knows(&id(2)),
            control,
            "{case}: a gone origin's record applies"
        );
    }
    crate::halt::NO_LIFT_ON_GONE.with(|c| c.set(false));
}

/// What the log's old halt rows did while the mesh-off bridge was down, before the upgrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhileDown {
    /// Nothing.
    Nothing,
    /// The person withdrew their halt.
    Withdrawn,
    /// Another person set a halt.
    Set,
}

/// **A HALT SET BEFORE THE UPGRADE HOLDS AFTER IT**: with the mesh off, a person's halt row on
/// the old subject (`/f/<F>/fleet/<h>/halt`) holds the Mac's session. The bridge starts again with
/// the mesh on, over the same state dir and log: the halt still holds, now under the id
/// [`carried_id`] names, said on the Mac's halt subject so every Mac learns it; the old row is
/// retired. A halt-off naming that id lifts it, and a later start carries nothing again. In force
/// is what the log's rows say: a halt withdrawn while the bridge was down is not carried, and one
/// set then is.
/// NEGATIVE CONTROL: [`crate::halt::NO_CARRY`] (the move carries none of the old halts): the
/// session runs free after the upgrade.
#[test]
fn a_halt_under_the_old_subject_holds_after_the_upgrade_and_its_id_lifts_it() {
    for (down, control) in [
        (WhileDown::Nothing, false),
        (WhileDown::Withdrawn, false),
        (WhileDown::Set, false),
        (WhileDown::Nothing, true),
    ] {
        crate::halt::NO_CARRY.with(|c| c.set(control));
        let case = format!("down={down:?} control={control}");
        let mac = Mac::new(31);
        mac.broker.set_origin_keys([mac.key]);
        let state = aterm_tempfile::tempdir().expect("a state dir");
        let (lane, mut before) =
            FakeVerbLane::bridge_configured(&[(1, "s-a")], Some(state.path()), None);
        let old_fleet = before.cfg.fleet.clone();
        let socket = lane.dir.path().join("bus.sock");
        let served_off = mac.broker.serve(&socket).expect("bus.log served");
        before.cfg.broker = socket.to_string_lossy().into_owned();
        before.caps = crate::enable::node_rules(&old_fleet, &before.node)
            .into_iter()
            .map(|grant| {
                let cap = astream_cap::mint(&mac.secret, &grant).expect("a grant");
                Cap {
                    grant: cap.filter,
                    tag: cap.tag,
                }
            })
            .collect();
        assert!(before.refresh_sessions().is_ok());
        let row = subject::person_halt(&old_fleet, "h-bob");
        mac.client(mac.key, &crate::enable::person_rules(&old_fleet, "h-bob"))
            .publish(pid("h-bob"), 1, &row, b"v=1 t=1 state=on reason=stop")
            .expect("the old halt row");
        assert!(
            before.attach_broker(),
            "the bridge attaches with the mesh off"
        );
        assert!(lane.holds("s-a"), "{case}: the old row holds");
        before.close_subscriptions();
        drop((before, lane));
        let carol = subject::person_halt(&old_fleet, "h-carol");
        match down {
            WhileDown::Nothing => {}
            WhileDown::Withdrawn => {
                mac.client(mac.key, &crate::enable::person_rules(&old_fleet, "h-bob"))
                    .publish(pid("h-bob"), 2, &row, b"v=1 t=2 state=off")
                    .expect("the old halt row withdrawn");
            }
            WhileDown::Set => {
                mac.client(mac.key, &crate::enable::person_rules(&old_fleet, "h-carol"))
                    .publish(pid("h-carol"), 1, &carol, b"v=1 t=2 state=on reason=also")
                    .expect("another old halt row");
            }
        }

        let keys = keys_for(31, Some(&members(&mac, &[mac.node.as_str()])));
        aterm_uds::private_file::write_private(
            keys.path(),
            crate::enable::MINT_SECRET,
            &mac.secret,
        )
        .expect("the root's mint secret");
        let (lane, mut bridge, served) = bridge_on(
            &mac,
            "s-a",
            keys.path(),
            Some(state.path()),
            crate::fx::Fx::os(),
        );
        *lane.outbox.lock().unwrap() = Some(String::new());
        assert!(
            bridge.attach_broker(),
            "the bridge attaches with the mesh on"
        );
        let carried = carried_id(&mac.node, &old_fleet, "h-bob");
        let mut want: Vec<(String, String)> = match down {
            _ if control => vec![],
            WhileDown::Nothing => vec![(carried.clone(), "h-bob".into())],
            WhileDown::Withdrawn => vec![],
            WhileDown::Set => vec![
                (carried.clone(), "h-bob".into()),
                (
                    carried_id(&mac.node, &old_fleet, "h-carol"),
                    "h-carol".into(),
                ),
            ],
        };
        want.sort();
        let mut seats = vec![Seat {
            bridge,
            lane,
            served: vec![served_off, served],
            sid: "s-a",
            state,
            keys,
            mac,
        }];
        assert_eq!(
            seats[0].held(),
            !want.is_empty(),
            "{case}: the old halt holds"
        );
        let mut said: Vec<(String, String)> = said_on(&seats[0])
            .into_iter()
            .map(|said| (said.id, said.by))
            .collect();
        said.sort();
        assert_eq!(said, want, "{case}: said on the Mac's halt subject");
        for row in [&row, &carol] {
            let (left, _) = seats[0]
                .mac
                .client(seats[0].mac.key, &[format!("ro:{row}")])
                .last_all(row)
                .expect("the old row's face");
            assert!(left.is_empty(), "{case}: the old row {row} is retired");
        }
        assert!(
            seats[0].bridge.state.old_standing_halts().is_empty(),
            "{case}: the old file is cleared"
        );
        if control || down != WhileDown::Nothing {
            continue;
        }
        ask(
            &seats[0].mac,
            "h-bob",
            2,
            &Ask {
                id: carried.clone(),
                word: Word::Off,
                here: false,
            },
        )
        .expect("the halt-off");
        settle(&mut seats, &format!("{case}: its id lifts it"), |s| {
            !s[0].held()
        });
        let a = seats.pop().expect("A");
        seats.push(a.restarted());
        assert!(!seats[0].held(), "{case}: nothing is carried again");
    }
    crate::halt::NO_CARRY.with(|c| c.set(false));
}

/// **A PERSON AT ANY MAC CAN LIFT A HALT THERE FOR THAT MAC** (§4.11): a halt a person set on A
/// holds A and B; a halt-off another person asks on B for B alone (`--here`) releases B, and A,
/// past a later record B said, still holds. Nothing of it is said on B's halt subject.
/// NEGATIVE CONTROL: the same halt-off asked for every Mac releases A too, so the check would
/// see a local lift that reached another Mac.
#[test]
fn a_halt_off_here_lifts_it_on_that_mac_alone() {
    for here in [true, false] {
        let case = format!("here={here}");
        let mut seats = world(&[75, 76], &["s-a", "s-b"]);
        let net = Arc::new(Net::default());
        let routes = relays(&seats, &net, &[(0, 1)]);
        ask(&seats[0].mac, "h-a", 1, &on(1)).expect("the halt");
        settle(&mut seats, &format!("{case}: both Macs hold"), |s| {
            s.iter().all(Seat::held)
        });
        ask(&seats[1].mac, "h-b", 1, &Ask { here, ..off(1) }).expect("the halt-off");
        settle(&mut seats, &format!("{case}: B lifts it"), |s| {
            !s[1].held() && s[1].bridge.mesh_halts.is_lifted(&id(1))
        });
        ask(&seats[1].mac, "h-b", 2, &off(9)).expect("a later record");
        settle(
            &mut seats,
            &format!("{case}: A read B's later record"),
            |s| s[0].bridge.mesh_halts.is_lifted(&id(9)),
        );
        assert_eq!(seats[0].held(), here, "{case}: A still holds");
        let b = &seats[1].mac;
        let said: Vec<String> = b
            .holds(&subject::halt_subject(FLEET, &b.node))
            .into_iter()
            .filter_map(|(_, _, body, _)| {
                Said::parse(astream_broker::origin::payload(&body)?)
                    .ok()
                    .map(|said| said.id)
            })
            .collect();
        assert_eq!(
            said.contains(&id(1)),
            !here,
            "{case}: B said the halt-off to every Mac"
        );
        if here {
            // The halt-off for every Mac, asked on B after its lift there, is no repeat of it.
            ask(&seats[1].mac, "h-b", 3, &off(1)).expect("the halt-off for every Mac");
            settle(&mut seats, &format!("{case}: then A lifts it"), |s| {
                !s[0].held() && s[0].bridge.mesh_halts.lifted_everywhere(&id(1))
            });
        }
        drop(routes);
    }
}

/// The halt records `seat`'s bridge said on its halt subject, in order.
fn said_on(seat: &Seat) -> Vec<Said> {
    seat.mac
        .holds(&subject::halt_subject(FLEET, &seat.mac.node))
        .into_iter()
        .filter_map(|(_, _, body, _)| Said::parse(astream_broker::origin::payload(&body)?).ok())
        .collect()
}

/// **A MAC THAT LIFTED A HALT FOR EVERY MAC ANSWERS ITS ORIGIN SAYING IT AGAIN**: X's halt holds
/// B, and B's person lifts it for every Mac. X, which never heard the halt-off, says its halt
/// again, twice: B says the halt-off back on its halt subject, once, as the person who asked
/// it, and stays released.
/// NEGATIVE CONTROL: lifted on B alone (`--here`), X's halt said again is no question for B:
/// nothing is said back.
#[test]
fn a_mac_that_lifted_a_halt_for_every_mac_answers_its_origin_saying_it_again() {
    for here in [false, true] {
        let case = format!("here={here}");
        let x = node_of(&key_of(68));
        let mut seats = world(&[67], &["s-b"]);
        let nodes = [seats[0].mac.node.as_str(), x.as_str()];
        members(&seats[0].mac, &nodes)
            .write(seats[0].keys.path())
            .expect("X is a member");
        let halt = |t_ms| Said {
            id: id(1),
            by: "h-x".to_owned(),
            word: Word::On("stop".to_owned()),
            t_ms,
        };
        said_by_node(&seats[0].mac, 68, 1, &halt(1));
        settle(&mut seats, &format!("{case}: B holds X's halt"), |s| {
            s[0].held()
        });
        ask(&seats[0].mac, "h-b", 1, &Ask { here, ..off(1) }).expect("the halt-off");
        settle(&mut seats, &format!("{case}: B lifts it"), |s| !s[0].held());
        let later = crate::fx::Fx::os().wall_ms() + 3_600_000;
        said_by_node(&seats[0].mac, 68, 2, &halt(later));
        said_by_node(&seats[0].mac, 68, 3, &halt(later + 1));
        let mark = Said {
            id: id(9),
            by: "h-x".to_owned(),
            word: Word::Off,
            t_ms: later + 2,
        };
        said_by_node(&seats[0].mac, 68, 4, &mark);
        settle(&mut seats, &format!("{case}: B read X's records"), |s| {
            s[0].bridge.mesh_halts.is_lifted(&id(9))
        });
        assert!(!seats[0].held(), "{case}: B stays released");
        let offs: Vec<String> = said_on(&seats[0])
            .into_iter()
            .filter(|said| said.id == id(1) && said.word == Word::Off)
            .map(|said| said.by)
            .collect();
        let want: &[&str] = if here { &[] } else { &["h-b", "h-b"] };
        assert_eq!(offs, want, "{case}: the halt-off said, and said back once");
    }
}

/// The asks `seat`'s bridge said for a halt it holds, in order.
fn asks_said_on(seat: &Seat) -> Vec<Said> {
    said_on(seat)
        .into_iter()
        .filter(|said| said.word == Word::Ask)
        .collect()
}

/// **A HALT-OFF REACHES A MAC BACK FROM HOWEVER LONG AWAY**, on an injected clock: X's halt
/// holds B and C, and B's person lifts it for every Mac while C is away; C comes back after
/// every log gave the halt-off up, so no record of it reaches C. Once X's newest saying of the
/// halt is [`crate::halt::STALE_MS`] old, C's bridge asks for it on its halt subject, never
/// before; B's, reaching the ask twice as its relay from C lands it, says the halt-off back once,
/// and C's, reaching that, lifts it.
/// NEGATIVE CONTROL: [`crate::halt::NO_STALE_ASK`] (a Mac never asks for another Mac's halt): C
/// asks nothing, so nothing is said back, and C holds still past a later record of B's.
#[test]
fn a_halt_off_reaches_a_mac_back_from_however_long_away() {
    use crate::halt::{NO_STALE_ASK, STALE_MS};
    for control in [false, true] {
        NO_STALE_ASK.with(|c| c.set(control));
        let case = format!("control={control}");
        let x = node_of(&key_of(78));
        let mut seats = world(&[76, 77], &["s-b", "s-c"]);
        let nodes = [
            seats[0].mac.node.as_str(),
            seats[1].mac.node.as_str(),
            x.as_str(),
        ];
        for seat in &seats {
            members(&seat.mac, &nodes)
                .write(seat.keys.path())
                .expect("X is a member");
        }
        let t = crate::fx::Fx::os().wall_ms();
        let halt = Said {
            id: id(1),
            by: "h-x".to_owned(),
            word: Word::On("stop".to_owned()),
            t_ms: t,
        };
        for seat in &seats {
            said_by_node(&seat.mac, 78, 1, &halt);
        }
        settle(&mut seats, &format!("{case}: B and C hold X's halt"), |s| {
            s.iter().all(Seat::held)
        });
        ask(&seats[0].mac, "h-b", 1, &off(1)).expect("the halt-off");
        settle(&mut seats, &format!("{case}: B lifts it"), |s| !s[0].held());
        let back = t + STALE_MS;
        assert!(seats[1].bridge.resay_due_halts(back - 1));
        assert_eq!(asks_said_on(&seats[1]), [], "{case}: not before");
        assert!(seats[1].bridge.resay_due_halts(back));
        let asks = asks_said_on(&seats[1]);
        if control {
            assert_eq!(asks, [], "{case}: C asks nothing");
            let mark = Said {
                id: id(9),
                by: "h-b".to_owned(),
                word: Word::Off,
                t_ms: back,
            };
            said_by_node(&seats[1].mac, 76, 1, &mark);
            settle(&mut seats, &format!("{case}: C read B's record"), |s| {
                s[1].bridge.mesh_halts.is_lifted(&id(9))
            });
            assert!(seats[1].held(), "{case}: C holds still");
            continue;
        }
        let asked = Said {
            id: id(1),
            by: "h-x".to_owned(),
            word: Word::Ask,
            t_ms: back,
        };
        assert_eq!(
            asks,
            std::slice::from_ref(&asked),
            "{case}: C asks for X's halt"
        );
        said_by_node(&seats[0].mac, 77, 1, &asked);
        let again = Said {
            t_ms: back + 1,
            ..asked
        };
        said_by_node(&seats[0].mac, 77, 2, &again);
        let offs = |seat: &Seat| -> Vec<Said> {
            said_on(seat)
                .into_iter()
                .filter(|said| said.id == id(1) && said.word == Word::Off)
                .collect()
        };
        settle(&mut seats, &format!("{case}: B answers C"), |s| {
            offs(&s[0]).len() == 2
        });
        let answer = offs(&seats[0]).pop().expect("the answer");
        assert_eq!(answer.by, "h-b", "{case}: as the person who asked it");
        said_by_node(&seats[1].mac, 76, 1, &answer);
        settle(&mut seats, &format!("{case}: C lifts it"), |s| !s[1].held());
        assert!(seats[1].bridge.mesh_halts.lifted_everywhere(&id(1)));
        let mark = Said {
            id: id(9),
            by: "h-b".to_owned(),
            word: Word::Off,
            t_ms: back,
        };
        said_by_node(&seats[0].mac, 77, 3, &mark);
        settle(
            &mut seats,
            &format!("{case}: B read C's later record"),
            |s| s[0].bridge.mesh_halts.is_lifted(&id(9)),
        );
        assert_eq!(offs(&seats[0]).len(), 2, "{case}: said back once");
    }
    NO_STALE_ASK.with(|c| c.set(false));
}

/// **A MAC SAYS ITS RECORDS AGAIN**, on an injected clock: a halt its person set is said again
/// once [`crate::halt::RESAY_MS`] passed since it was said, never before; lifted for every Mac by
/// its person, the halt-off is said again in turn, and nothing once
/// [`crate::halt::OFF_RESAY_SPAN_MS`] passed since the lift. So a Mac away while a record went
/// round, and back after every log gave it up, hears it.
#[test]
fn a_mac_says_its_halt_and_its_halt_off_again_on_their_clock() {
    use crate::halt::{OFF_RESAY_SPAN_MS, RESAY_MS};
    let mut seats = world(&[65], &["s-a"]);
    ask(&seats[0].mac, "h-a", 1, &on(1)).expect("the halt");
    settle(&mut seats, "the halt holds", |s| s[0].held());
    let first = said_on(&seats[0]);
    assert_eq!(first.len(), 1, "{first:?}");
    let t = first[0].t_ms;
    assert!(seats[0].bridge.resay_due_halts(t + RESAY_MS - 1));
    assert_eq!(said_on(&seats[0]).len(), 1, "not before");
    assert!(seats[0].bridge.resay_due_halts(t + RESAY_MS));
    assert_eq!(
        said_on(&seats[0])[1..],
        [Said {
            t_ms: t + RESAY_MS,
            ..first[0].clone()
        }],
        "said again"
    );
    ask(&seats[0].mac, "h-a", 2, &off(1)).expect("the halt-off");
    settle(&mut seats, "the halt-off releases", |s| !s[0].held());
    let lifted = said_on(&seats[0]);
    let off = lifted.last().expect("the halt-off").clone();
    assert_eq!(off.word, Word::Off);
    let at = seats[0].bridge.mesh_halts.lifted()[&id(1)].at_ms;
    assert!(seats[0].bridge.resay_due_halts(off.t_ms + RESAY_MS));
    assert_eq!(
        said_on(&seats[0])[lifted.len()..],
        [Said {
            t_ms: off.t_ms + RESAY_MS,
            ..off
        }],
        "the halt-off said again"
    );
    assert!(seats[0].bridge.resay_due_halts(at + OFF_RESAY_SPAN_MS));
    assert_eq!(
        said_on(&seats[0]).len(),
        lifted.len() + 1,
        "nothing past the span"
    );
}

/// **A RELAY HOLDS A HALT THE FAR END DOES NOT TAKE YET**: N's halt is on A's log, and D does not
/// seat N yet, so A's relay to D holds the record while A's log seats N, carries A's own later
/// halt past it, and D takes N's once it seats N.
/// NEGATIVE CONTROL: A's log no longer seats N, so the relay drops N's record: D never takes it,
/// though it seats N later and takes a halt A said after.
#[test]
fn a_relay_holds_a_halt_the_far_end_does_not_take_yet() {
    for control in [false, true] {
        let case = format!("control={control}");
        let mut seats = world(&[45, 46], &["s-a", "s-d"]);
        let n = node_of(&key_of(47));
        let (a_key, d_key) = (seats[0].mac.key, seats[1].mac.key);
        seats[1].mac.broker.set_origin_keys([a_key, d_key]);
        let net = Arc::new(Net::default());
        let held_n = n.clone();
        let routes = crate::federate::tests::routes_seating(
            &seats[0].mac,
            vec![Peer::of(&seats[1].mac)],
            &net,
            false,
            Arc::new(move |owner| owner != held_n || !control),
        );
        let nodes = [
            seats[0].mac.node.as_str(),
            seats[1].mac.node.as_str(),
            n.as_str(),
        ];
        routes.set(&members(&seats[0].mac, &nodes), &BTreeSet::new());
        let halt = Said {
            id: id(1),
            by: "h-n".to_owned(),
            word: Word::On("stop".to_owned()),
            t_ms: 1,
        };
        said_by_node(&seats[0].mac, 47, 1, &halt);
        ask(&seats[0].mac, "h-a", 1, &on(2)).expect("A's own halt");
        settle(&mut seats, &format!("{case}: D takes A's halt"), |s| {
            s[1].bridge.mesh_halts.knows(&id(2))
        });
        assert!(
            !seats[1].bridge.mesh_halts.knows(&id(1)),
            "{case}: D takes N's halt before it seats N"
        );
        seats[1]
            .mac
            .broker
            .set_origin_keys([a_key, d_key, key_of(47)]);
        if control {
            ask(&seats[0].mac, "h-a", 2, &on(3)).expect("A's later halt");
            settle(
                &mut seats,
                &format!("{case}: D takes A's later halt"),
                |s| s[1].bridge.mesh_halts.knows(&id(3)),
            );
            assert!(
                !seats[1].bridge.mesh_halts.knows(&id(1)),
                "{case}: the dropped record never lands"
            );
        } else {
            settle(&mut seats, &format!("{case}: D takes N's halt"), |s| {
                s[1].bridge.mesh_halts.knows(&id(1))
            });
        }
        drop(routes);
    }
}

/// **A REQUEST THAT CANNOT BE SAID WAITS, THE BRIDGE ATTACHED**: the node key on disk names
/// another node (a rotation the bridge has not restarted for), so the bridge says nothing under
/// its node's name. A person's halt is kept unsaid while the link stays up, the bridge attached,
/// and said once the key names the bridge's node again.
#[test]
fn a_request_that_cannot_be_said_waits_and_the_bridge_stays_attached() {
    let mut seats = world(&[43], &["s-a"]);
    let seed = |tag: u8| format!("{}\n", aterm_codec::hex::encode_lower(&[tag; 32]));
    let write_key = |seat: &Seat, tag: u8| {
        aterm_uds::private_file::write_private(
            seat.keys.path(),
            crate::federate::NODE_KEY_FILE,
            seed(tag).as_bytes(),
        )
        .expect("the node key");
    };
    write_key(&seats[0], 44);
    ask(&seats[0].mac, "h-a", 1, &on(1)).expect("the halt");
    settle(&mut seats, "the request is kept unsaid", |s| {
        !s[0].bridge.halt_unsaid.is_empty()
    });
    assert!(seats[0].bridge.conn.is_some(), "the bridge stays attached");
    assert!(!seats[0].held(), "nothing was said, so nothing holds");
    write_key(&seats[0], 43);
    assert!(
        seats[0].bridge.say_unsaid_asks(),
        "said once the key is back"
    );
    settle(&mut seats, "the halt holds", |s| s[0].held());
    assert_eq!(said_on(&seats[0]).len(), 1);
}
