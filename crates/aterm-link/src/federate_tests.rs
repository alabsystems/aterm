// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fabric over the mesh (§11 C3): its names, its hand-off, its plan, and its pushes between
//! in-process `bus.log` brokers, each guarded by its own mint secret and serving the others the
//! ring of §4.11 over adopted streams, as the node service serves them over a link. A partition
//! shuts a peer's streams and refuses new ones. Every wait is the one-minute hang detector.

use std::collections::BTreeMap;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::Instant;

use astream_broker::{AnyClient, Broker, StreamLink};

use super::*;

const MUST: Duration = aterm_spec::wait::MUST_HAPPEN;

pub(crate) fn key_of(tag: u8) -> [u8; 32] {
    astream_broker::origin::public_key(&[tag; 32])
}

pub(crate) fn pid(node: &str) -> u64 {
    astream_cap::producer_id_of(node)
}

/// A note from session `s-1` on `src` to session `s-1` on `dst`.
pub(crate) fn lane(dst: &str, src: &str) -> String {
    format!("/f/{FLEET}/in/{dst}/s-1/{src}/note")
}

/// Wait until `done`, which must happen.
pub(crate) fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + MUST;
    while !done() {
        assert!(Instant::now() < deadline, "{what}: not within {MUST:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_lanes_are_the_one_fleets_and_the_push_follows_the_mail_rules_reader() {
    let (me, dst) = ("n-00000000000000aa", "n-00000000000000bb");
    assert_eq!(mail_lanes(dst, me), format!("/f/alab/in/{dst}/*/{me}/*"));
    assert_eq!(written_lanes(me), format!("/f/alab/in/*/*/{me}/*"));
    // The mail rule names a lane's reader by its fleet and addressee segments.
    let reader = crate::broker_rules::MAIL_READER
        .replace("{2}", FLEET)
        .replace("{4}", dst);
    assert_eq!(mail_group(dst), reader);
    assert_eq!(
        Lane::Mail { dst: dst.into() }.group(me),
        reader,
        "a mail push follows under the group the mail rule holds the lane for"
    );
    assert_eq!(mail_lane(&lane(dst, me)), Some((dst, me)));
    for not_a_nodes in [
        lane(dst, "h-owner"),
        lane("h-owner", me),
        format!("/f/local/in/{dst}/s-1/{me}/note"),
        format!("/f/{FLEET}/in/{dst}/s-1/{me}"),
        format!("/f/{FLEET}/in/{dst}/s-1/{me}/note/more"),
    ] {
        assert_eq!(mail_lane(&not_a_nodes), None, "{not_a_nodes}");
    }
    // A push carries its own lanes exactly, and with retired nodes follows its group over every
    // lane to the addressee and carries each of its nodes' lanes within it.
    let mail = Lane::Mail { dst: dst.into() };
    assert_eq!(
        mail.records(me, &BTreeSet::new(), false),
        (mail_lanes(dst, me), Vec::new())
    );
    let old = "n-00000000000000a0";
    let olds = BTreeSet::from([old.to_owned()]);
    assert_eq!(
        mail.records(me, &olds, false),
        (
            format!("/f/alab/in/{dst}/*/*/*"),
            vec![mail_lanes(dst, me), mail_lanes(dst, old)]
        )
    );
    let moved = Lane::Move {
        old: "n-00000000000000b0".into(),
        new: dst.into(),
    };
    assert_eq!(
        moved.records(me, &olds, false).1,
        vec![
            mail_lanes("n-00000000000000b0", me),
            mail_lanes("n-00000000000000b0", old)
        ]
    );
    assert_eq!(
        Lane::Say { dst: dst.into() }.records(me, &olds, false),
        (say_lanes(me), Vec::new()),
        "topics written under a retired node are not carried"
    );
    // The far end's ring takes the peer's mail under its retired nodes, and only to itself.
    let ring = peer_ring(FLEET, dst, me, &olds);
    assert_eq!(ring.len(), 6);
    assert_eq!(ring[0], format!("rw,p={me}:/f/alab/in/{dst}/*/{me}/*"));
    assert_eq!(ring[5], format!("rw,p={me}:/f/alab/in/{dst}/*/{old}/*"));
    let many: BTreeSet<String> = (0..20).map(|i| format!("n-{i:016x}")).collect();
    assert_eq!(
        peer_ring(FLEET, dst, me, &many).len(),
        5 + RING_OLDS_MAX,
        "bounded"
    );
    assert!(is_node(me) && !is_node("n-AA00000000000000") && !is_node("n-aa"));
}

/// A topics face names the topics in order and reads back as them; a body without the word, or
/// one naming a word outside the topic grammar, names none rather than a guess.
#[test]
fn a_topics_face_reads_back_as_the_topics_it_names() {
    let me = "n-00000000000000aa";
    assert_eq!(topics_face(me), format!("/f/alab/pub/{me}/node/topics"));
    let topics = BTreeSet::from(["deploy".to_owned(), "build".to_owned()]);
    let body = topics_body(7, &topics);
    assert_eq!(body, "v=1 t=7 topics=build,deploy");
    assert_eq!(topics_of(body.as_bytes()), Some(topics));
    assert_eq!(
        topics_of(topics_body(7, &BTreeSet::new()).as_bytes()),
        Some(BTreeSet::new())
    );
    for refused in [
        &b"v=1 t=7"[..],
        &b"v=1 topics=build,No"[..],
        &b"topics=a/b"[..],
        &b"\xff"[..],
    ] {
        assert_eq!(
            topics_of(refused),
            None,
            "{}",
            String::from_utf8_lossy(refused)
        );
    }
}

#[test]
fn a_hand_off_reads_back_and_follows_each_rotation_to_a_member() {
    let handoff = Handoff {
        me: "n-000000000000000a".into(),
        members: ["n-000000000000000a", "n-000000000000000c"]
            .map(String::from)
            .into(),
        moved: [
            ("n-00000000000000b1".into(), "n-00000000000000b2".into()),
            ("n-00000000000000b2".into(), "n-000000000000000c".into()),
        ]
        .into(),
        rank: [
            ("n-00000000000000b1".into(), 1),
            ("n-00000000000000b2".into(), 0),
        ]
        .into(),
        gone: ["n-00000000000000b1", "n-00000000000000dd"]
            .map(String::from)
            .into(),
    };
    let text = handoff.render();
    assert!(text.contains("gone n-00000000000000dd\n"), "{text}");
    assert!(text.contains("rank n-00000000000000b2 0\n"), "{text}");
    assert_eq!(Handoff::parse(&text), Ok(handoff.clone()));
    assert_eq!(
        Handoff::parse(&format!("{text}later a word this build does not know\n")),
        Ok(handoff.clone()),
        "a later node service may say more"
    );
    // A reader that predates `rank` skips its lines, and reads the rest alike.
    let unranked: String = text
        .lines()
        .filter(|line| !line.starts_with("rank "))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(
        Handoff::parse(&unranked),
        Ok(Handoff {
            rank: BTreeMap::new(),
            ..handoff.clone()
        })
    );
    for bad in [
        format!("{text}member n-xyz\n"),
        format!("{text}moved n-000000000000000a\n"),
        format!("{text}gone n-xyz\n"),
        format!("{text}rank n-xyz 0\n"),
        format!("{text}rank n-00000000000000b1\n"),
        format!("{text}rank n-00000000000000b1 x\n"),
        format!("{text}rank n-00000000000000b1 01\n"),
        format!("{text}rank n-00000000000000b1 +1\n"),
        format!("{text}rank n-00000000000000b1 -1\n"),
        format!("{text}rank n-00000000000000b1 1 2\n"),
        "mesh-handoff v2\nme n-000000000000000a\n".to_owned(),
        "mesh-handoff v1\nmember n-000000000000000a\n".to_owned(),
    ] {
        assert!(Handoff::parse(&bad).is_err(), "{bad}");
    }
    assert_eq!(
        handoff.current("n-00000000000000b1"),
        Some("n-000000000000000c")
    );
    assert_eq!(
        handoff.current("n-000000000000000c"),
        Some("n-000000000000000c")
    );
    assert_eq!(handoff.current("n-00000000000000dd"), None);
    let cycle = Handoff {
        moved: [
            ("n-00000000000000e1".into(), "n-00000000000000e2".into()),
            ("n-00000000000000e2".into(), "n-00000000000000e1".into()),
        ]
        .into(),
        ..handoff.clone()
    };
    assert_eq!(cycle.current("n-00000000000000e1"), None, "a cycle ends");
    assert_eq!(
        handoff.retired("n-000000000000000c"),
        BTreeSet::from([
            "n-00000000000000b1".to_owned(),
            "n-00000000000000b2".to_owned()
        ]),
        "every node a seat held before, through each rotation"
    );
    assert!(handoff.retired("n-000000000000000a").is_empty());
    assert!(
        cycle.retired("n-00000000000000e1").is_empty(),
        "a cycle names no seat"
    );

    let dir = crate::test_scratch_dir("fed");
    aterm_uds::private_file::ensure_private_dir(dir.path()).expect("a private dir");
    assert_eq!(Handoff::read(dir.path()).expect("none"), None);
    handoff.write(dir.path()).expect("written");
    assert_eq!(Handoff::read(dir.path()).expect("read"), Some(handoff));
}

/// NEGATIVE CONTROL: with [`FORWARD_ALL`]'s rule every member gets a mail push.
#[test]
fn the_plan_pushes_only_to_members_this_mac_wrote_to() {
    let me = "n-000000000000000a";
    let handoff = Handoff {
        me: me.into(),
        members: [me, "n-000000000000000b", "n-000000000000000c"]
            .map(String::from)
            .into(),
        moved: [("n-00000000000000b0".into(), "n-000000000000000b".into())].into(),
        rank: BTreeMap::new(),
        gone: BTreeSet::new(),
    };
    let written = [
        me,
        "n-000000000000000b",
        "n-00000000000000b0",
        "n-00000000000000ff",
    ]
    .map(String::from)
    .into();
    let holders = ["n-000000000000000c", "n-00000000000000ff"]
        .map(String::from)
        .into();
    let mail = |dst: &str| Lane::Mail { dst: dst.into() };
    assert_eq!(
        plan(me, &BTreeSet::new(), &handoff, &holders, &written, false),
        [
            mail("n-000000000000000b"),
            Lane::Move {
                old: "n-00000000000000b0".into(),
                new: "n-000000000000000b".into()
            },
            Lane::Say {
                dst: "n-000000000000000c".into()
            },
        ]
        .into(),
        "no push to itself, to a node that is no member, or to a member it wrote nothing"
    );
    assert!(
        plan(me, &BTreeSet::new(), &handoff, &holders, &written, true)
            .contains(&mail("n-000000000000000c")),
        "the control pushes to every member"
    );
    let olds = BTreeSet::from(["n-00000000000000a0".to_owned()]);
    assert!(
        plan(
            me,
            &olds,
            &handoff,
            &BTreeSet::new(),
            &BTreeSet::new(),
            false
        )
        .contains(&Lane::Inbox {
            old: "n-00000000000000a0".into()
        }),
        "a retired node of this Mac's own gets its unread mail moved, written to or not"
    );
}

/// Halts go to every other member, whatever links the overlay holds: a member whose links host
/// no `bus.log` cuts no Mac off (§11 C4). A gone node, or one that is no member, gets none, and a
/// Mac the log took out sends none.
#[test]
fn the_halts_go_to_every_member_but_this_mac() {
    let me = "n-000000000000000a";
    let handoff = Handoff {
        me: me.into(),
        members: [me, "n-000000000000000b", "n-000000000000000c"]
            .map(String::from)
            .into(),
        moved: BTreeMap::new(),
        rank: BTreeMap::new(),
        gone: ["n-00000000000000ff".to_string()].into(),
    };
    let halts = |dst: &str| Lane::Halts { dst: dst.into() };
    assert_eq!(
        halt_lanes(me, &handoff),
        [halts("n-000000000000000b"), halts("n-000000000000000c")].into()
    );
    assert!(halt_lanes(me, &Handoff::default()).is_empty());
    let out = Handoff {
        members: ["n-000000000000000b", "n-000000000000000c"]
            .map(String::from)
            .into(),
        ..handoff
    };
    assert!(
        halt_lanes(me, &out).is_empty(),
        "none from a Mac the log took out"
    );
}

#[test]
fn the_node_key_names_the_node() {
    let dir = crate::test_scratch_dir("fed");
    aterm_uds::private_file::ensure_private_dir(dir.path()).expect("a private dir");
    assert_eq!(node_of_key(dir.path()).expect("no key"), None);
    let seed = [7u8; 32];
    let text = format!("{}\n", aterm_codec::hex::encode_lower(&seed));
    aterm_uds::private_file::write_private(dir.path(), NODE_KEY_FILE, text.as_bytes())
        .expect("the key");
    let node = node_of_key(dir.path()).expect("a key").expect("a node");
    assert_eq!(
        node,
        astream_broker::identity_principal(&astream_broker::origin::public_key(&seed))
    );
    assert!(is_node(&node));
    aterm_uds::private_file::write_private(dir.path(), NODE_KEY_FILE, b"not a key\n")
        .expect("a bad key");
    assert!(node_of_key(dir.path()).is_err());
}

/// A record a Mac holds: its offset, subject, body and the (producer, sequence) it carries.
pub(crate) type HeldRecord = (u64, String, Vec<u8>, Option<(u64, u64)>);

/// One Mac: its `bus.log` under aterm's rules, guarded by its own mint secret.
pub(crate) struct Mac {
    pub(crate) key: [u8; 32],
    pub(crate) node: String,
    pub(crate) secret: Vec<u8>,
    pub(crate) broker: Arc<Broker>,
    /// While set, the node service's mint refuses the person carry's grants: the carry, at its
    /// next attach, cannot follow ([`routes_noting`]).
    pub(crate) carry_down: Arc<std::sync::atomic::AtomicBool>,
    dir: Arc<aterm_tempfile::TempDir>,
}

impl Mac {
    pub(crate) fn new(tag: u8) -> Self {
        let dir = crate::test_scratch_dir("fed");
        let secret = vec![tag ^ 0x5a; 32];
        let broker = Broker::open_guarded_with(
            dir.path().join("bus.log"),
            secret.clone(),
            astream_broker::Durability::Relaxed,
        )
        .expect("bus.log");
        broker
            .set_retention(crate::broker_rules::default_rules())
            .expect("aterm's rules");
        let key = key_of(tag);
        Self {
            key,
            node: node_of(&key),
            secret,
            broker: Arc::new(broker),
            carry_down: Arc::default(),
            dir: Arc::new(dir),
        }
    }

    /// This Mac after a key rotation, as its bridge sees it: the same log and mint secret,
    /// under key `tag` and the node it names.
    pub(crate) fn as_key(&self, tag: u8) -> Self {
        let key = key_of(tag);
        Self {
            key,
            node: node_of(&key),
            secret: self.secret.clone(),
            broker: Arc::clone(&self.broker),
            carry_down: Arc::clone(&self.carry_down),
            dir: Arc::clone(&self.dir),
        }
    }

    /// Where this Mac's fabric bridge keeps its state, beside its `bus.log`, as aterm launches it
    /// ([`BRIDGE_STATE_DIR`]): what its node service's own-inbox move reads.
    pub(crate) fn link_state(&self) -> std::path::PathBuf {
        self.dir.path().join(BRIDGE_STATE_DIR)
    }

    /// This Mac's bus.log after a key rotation: the same log, a new key.
    pub(crate) fn rekeyed(&self, tag: u8) -> (String, [u8; 32]) {
        let key = key_of(tag);
        (node_of(&key), key)
    }

    /// This Mac after its node key rotated to Mac `tag`'s, as its bridge sees it: the same
    /// `bus.log` under the same secret, the new key's node.
    pub(crate) fn rotated(&self, tag: u8) -> Self {
        let key = key_of(tag);
        Self {
            key,
            node: node_of(&key),
            secret: self.secret.clone(),
            broker: Arc::clone(&self.broker),
            carry_down: Arc::clone(&self.carry_down),
            dir: Arc::new(crate::test_scratch_dir("fed")),
        }
    }

    pub(crate) fn mint(secret: &[u8], grant: &str) -> io::Result<Capability> {
        astream_broker::mint(secret, grant)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    /// A client of this Mac's `bus.log`, adopted as `key`, holding `grants`.
    pub(crate) fn client(&self, key: [u8; 32], grants: &[String]) -> AnyClient {
        let (ours, theirs) = UnixStream::pair().expect("a pair");
        drop(
            self.broker
                .serve_stream(Arc::new(theirs), key)
                .expect("adopted"),
        );
        ours.set_read_timeout(Some(MUST)).expect("a bound");
        let (mut client, _closer) = astream_broker::connect_stream(Arc::new(ours));
        for grant in grants {
            let cap = Self::mint(&self.secret, grant).expect("a grant");
            client.attach(&cap.filter, &cap.tag).expect("attached");
        }
        client
    }

    /// A note to `dst`, written as this Mac's bridge writes it: under its producer id.
    pub(crate) fn post(&self, dst: &str, seq: u64, body: &[u8]) -> u64 {
        let mut client = self.client(
            self.key,
            &[format!("rw,p={}:{}", self.node, written_lanes(&self.node))],
        );
        client
            .publish(pid(&self.node), seq, &lane(dst, &self.node), body)
            .expect("the bridge's post lands")
            .0
    }

    /// Every record of `filter` this Mac holds.
    pub(crate) fn holds(&self, filter: &str) -> Vec<HeldRecord> {
        let mut client = self.client(self.key, &[format!("ro:{filter}")]);
        let (rows, _) = match client.fetch(0, filter, 1024) {
            Ok(page) => page,
            Err(e) => {
                let from = astream_broker::broker_refusal(&e)
                    .and_then(astream_broker::BrokerRefusal::retained_from)
                    .unwrap_or_else(|| panic!("a read of {filter}: {e}"));
                client
                    .fetch(from, filter, 1024)
                    .expect("a read from the oldest")
            }
        };
        let Some(first) = rows.first().map(|row| row.0) else {
            return Vec::new();
        };
        assert!(client.origin_stamps().expect("stamps"), "origin stamps");
        let mut sub = client.subscribe(first, filter).expect("a tail");
        let mut out = Vec::new();
        while out.len() < rows.len() {
            match sub.recv_event().expect("a delivery") {
                Some(astream_broker::Event::Delivery {
                    offset,
                    subject,
                    body,
                }) => out.push((offset, subject, body, sub.last_origin())),
                Some(_) => {}
                None => panic!("the tail of {filter} ended"),
            }
        }
        out
    }

    /// The bodies of the mail this Mac holds for `node`.
    pub(crate) fn mail_for(&self, node: &str) -> Vec<Vec<u8>> {
        self.holds(&format!("/f/{FLEET}/in/{node}/>"))
            .into_iter()
            .map(|(_, _, body, _)| body)
            .collect()
    }
}

/// The streams between the Macs: those a push opened to each, so a partition shuts them, and
/// which Macs a partition cuts off.
#[derive(Default)]
pub(crate) struct Net {
    cut: Mutex<BTreeSet<String>>,
    streams: Mutex<BTreeMap<String, Vec<Arc<UnixStream>>>>,
}

impl Net {
    pub(crate) fn cut(&self, node: &str) {
        self.cut.lock_or_recover().insert(node.to_owned());
        for stream in self
            .streams
            .lock_or_recover()
            .remove(node)
            .unwrap_or_default()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub(crate) fn heal(&self, node: &str) {
        self.cut.lock_or_recover().remove(node);
    }

    fn refuses(&self, node: &str) -> io::Result<()> {
        if self.cut.lock_or_recover().contains(node) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("{node} is cut off"),
            ));
        }
        Ok(())
    }
}

/// Streams a broker adopts under `key`; to a peer (`to`), recorded and refused while cut.
pub(crate) struct Adopt {
    broker: Arc<Broker>,
    key: [u8; 32],
    to: Option<(Arc<Net>, String)>,
}

impl StreamDial for Adopt {
    fn dial(&self) -> io::Result<Arc<dyn StreamLink>> {
        if let Some((net, node)) = &self.to {
            net.refuses(node)?;
        }
        let (ours, theirs) = UnixStream::pair()?;
        drop(self.broker.serve_stream(Arc::new(theirs), self.key)?);
        let ours = Arc::new(ours);
        if let Some((net, node)) = &self.to {
            net.streams
                .lock_or_recover()
                .entry(node.clone())
                .or_default()
                .push(Arc::clone(&ours));
        }
        Ok(ours)
    }
}

/// A peer as a push reaches it: the node it proves, its broker, and the secret its ring is
/// minted under.
#[derive(Clone)]
pub(crate) struct Peer {
    pub(crate) node: String,
    pub(crate) broker: Arc<Broker>,
    pub(crate) secret: Vec<u8>,
}

impl Peer {
    pub(crate) fn of(mac: &Mac) -> Self {
        Self {
            node: mac.node.clone(),
            broker: Arc::clone(&mac.broker),
            secret: mac.secret.clone(),
        }
    }
}

/// `from`'s pushes to `peers` over `net`. With `wide` each peer serves `from` the mail grant of
/// the ring before C3, which took a peer's mail for any Mac.
pub(crate) fn routes_of(from: &Mac, peers: Vec<Peer>, net: &Arc<Net>, wide: bool) -> Routes {
    routes_as(
        from,
        (&from.node, from.key),
        &BTreeSet::new(),
        peers,
        net,
        wide,
    )
}

/// [`routes_of`], this Mac's log seating the nodes `seats` names.
pub(crate) fn routes_seating(
    from: &Mac,
    peers: Vec<Peer>,
    net: &Arc<Net>,
    wide: bool,
    seats: SeatsFn,
) -> Routes {
    routes_noting(
        from,
        (&from.node, from.key),
        &BTreeSet::new(),
        (peers, net, wide),
        Arc::new(|_| {}),
        seats,
    )
}

/// [`routes_of`] for `from` under the node and key `(me, key)`, whose seat the peers know held
/// `olds` before: what a rotated Mac's node service runs on the same `bus.log`.
pub(crate) fn routes_as(
    from: &Mac,
    me: (&str, [u8; 32]),
    olds: &BTreeSet<String>,
    peers: Vec<Peer>,
    net: &Arc<Net>,
    wide: bool,
) -> Routes {
    routes_noting(
        from,
        me,
        olds,
        (peers, net, wide),
        Arc::new(|_| {}),
        Arc::new(|_| true),
    )
}

/// [`routes_as`], journaling to `note`, this Mac's log seating the nodes `seats` names, its mint
/// refusing the person carry's grants while [`Mac::carry_down`] is set.
pub(crate) fn routes_noting(
    from: &Mac,
    (me, key): (&str, [u8; 32]),
    olds: &BTreeSet<String>,
    (peers, net, wide): (Vec<Peer>, &Arc<Net>, bool),
    note: NoteFn,
    seats: SeatsFn,
) -> Routes {
    let me = me.to_owned();
    let olds = olds.clone();
    let secret = from.secret.clone();
    let net = Arc::clone(net);
    let bridge_state = from.link_state();
    let carry_down = Arc::clone(&from.carry_down);
    Routes::start(RoutesConfig {
        me: me.clone(),
        key,
        source: Arc::new(Adopt {
            broker: Arc::clone(&from.broker),
            key,
            to: None,
        }),
        mint: Arc::new(move |grant| {
            let carrier = format!("p={}:", crate::federate::CARRIER);
            if carry_down.load(std::sync::atomic::Ordering::SeqCst) && grant.contains(&carrier) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the person carry is down",
                ));
            }
            Mac::mint(&secret, grant)
        }),
        dest: Arc::new(move |node| {
            net.refuses(node)?;
            let peer = peers
                .iter()
                .find(|peer| peer.node == node)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such Mac"))?;
            let mut ring = peer_ring(FLEET, &peer.node, &me, &olds);
            if wide {
                ring[0] = format!("rw,p={me}:/f/{FLEET}/in/*/*/{me}/*");
            }
            Ok(Dest {
                dial: Arc::new(Adopt {
                    broker: Arc::clone(&peer.broker),
                    key,
                    to: Some((Arc::clone(&net), node.to_owned())),
                }),
                grants: ring
                    .iter()
                    .map(|grant| Mac::mint(&peer.secret, grant))
                    .collect::<io::Result<_>>()?,
            })
        }),
        note,
        seats,
        bridge_state: Arc::new(move || Some(bridge_state.clone())),
    })
    .expect("the routes start")
}

/// The own-inbox move of `old`'s unread mail to `me`'s node on `mac`'s `bus.log`, as the node
/// service's push runs it ([`Lane::Inbox`], gated on the bridge state dir `state` and opened by
/// [`Shared::open`]): one burst, carried and committed, settled when it returns.
pub(crate) fn move_once(mac: &Mac, me: &Mac, old: &str, state: &std::path::Path) -> io::Result<()> {
    let lane = Lane::Inbox {
        old: old.to_owned(),
    };
    let secret = mac.secret.clone();
    let state = state.to_path_buf();
    let shared = Shared {
        cfg: RoutesConfig {
            me: me.node.clone(),
            key: me.key,
            source: Arc::new(Adopt {
                broker: Arc::clone(&mac.broker),
                key: me.key,
                to: None,
            }),
            mint: Arc::new(move |grant| Mac::mint(&secret, grant)),
            dest: Arc::new(|_| Err(io::Error::new(io::ErrorKind::NotFound, "no other Mac"))),
            note: Arc::new(|_| {}),
            seats: Arc::new(|_| true),
            bridge_state: Arc::new(move || Some(state.clone())),
        },
        forward_all: false,
        another_group: false,
        own_halts_only: false,
        one_node: false,
        node_order_olds: false,
        node_order_ring: false,
        unsealed_carry: false,
        ungated_inbox: false,
        give_back_carried: false,
        state: Mutex::new(State::default()),
        bell: Condvar::new(),
    };
    let group = shared.group_of(&lane);
    let (mut bridge, _ungranted) = shared.open(&lane, &group, &BTreeSet::from([old.to_owned()]))?;
    bridge.sync_once(256).map(|_| ()).map_err(io_of)
}

/// The lanes `routes` runs besides the halt relays, which run to every member.
fn pushes(routes: &Routes) -> Vec<Lane> {
    routes
        .status()
        .into_keys()
        .filter(|lane| !matches!(lane, Lane::Halts { .. }))
        .collect()
}

pub(crate) fn members(me: &Mac, of: &[&str]) -> Handoff {
    Handoff {
        me: me.node.clone(),
        members: of.iter().map(|node| (*node).to_owned()).collect(),
        moved: BTreeMap::new(),
        rank: BTreeMap::new(),
        gone: BTreeSet::new(),
    }
}

/// **A POST GOES ONE HOP, TO ITS ADDRESSEE ONLY**: A's note to B lands on B's `bus.log` once,
/// under A's producer and numbered by its offset on A (so B's answer can name it there), and A's
/// group for B commits past it once B acknowledged it. C, a member A wrote nothing to, gets no
/// push and holds nothing.
/// NEGATIVE CONTROL: [`FORWARD_ALL`] with C serving the mail grant of the ring before C3 (a
/// peer's mail for any Mac): A's note to B lands on C too.
#[test]
fn mail_goes_one_hop_to_the_mac_it_names_and_no_third() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (a, b, c) = (Mac::new(1), Mac::new(2), Mac::new(3));
        let net = Arc::new(Net::default());
        FORWARD_ALL.with(|on| on.set(control));
        let routes = routes_of(&a, vec![Peer::of(&b), Peer::of(&c)], &net, control);
        FORWARD_ALL.with(|on| on.set(false));
        routes.set(&members(&a, &[&a.node, &b.node, &c.node]), &BTreeSet::new());
        let at = a.post(&b.node, 1, b"hello b");
        until(&format!("{case}: B holds A's note"), || {
            !b.mail_for(&b.node).is_empty()
        });
        let held = b.holds(&format!("/f/{FLEET}/in/{}/>", b.node));
        assert_eq!(held.len(), 1, "{case}: once");
        let (_, subject, body, origin) = &held[0];
        assert_eq!(subject, &lane(&b.node, &a.node), "{case}");
        assert_eq!(body, b"hello b", "{case}");
        assert_eq!(
            *origin,
            Some((pid(&a.node), at)),
            "{case}: under A's producer, numbered by its offset on A"
        );
        until(&format!("{case}: A's group for B passes the note"), || {
            a.broker.group_start(&mail_group(&b.node)) > at
        });
        if control {
            until(&format!("{case}: the leak reaches C"), || {
                !c.mail_for(&b.node).is_empty()
            });
        } else {
            assert_eq!(
                pushes(&routes),
                [Lane::Mail {
                    dst: b.node.clone()
                }],
                "{case}: one push, to B"
            );
            assert!(c.mail_for(&b.node).is_empty(), "{case}: C holds nothing");
        }
        drop(routes);
    }
}

/// **A TOPIC BROADCAST GOES ONE HOP, TO THE MACS THAT HOLD A TOPIC**: B's row names a topic and
/// C's none, so A's broadcast lands on B once, under A's producer and numbered by its offset on A,
/// and A's say group for B passes it once B acknowledged it; C gets no push and holds nothing.
/// NEGATIVE CONTROL: C named a holder too: the same broadcast reaches C, so a leak is seen.
#[test]
fn a_topic_broadcast_goes_one_hop_to_the_macs_that_hold_a_topic() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (a, b, c) = (Mac::new(31), Mac::new(32), Mac::new(33));
        let net = Arc::new(Net::default());
        let routes = routes_of(&a, vec![Peer::of(&b), Peer::of(&c)], &net, false);
        let mut holders = BTreeSet::from([b.node.clone()]);
        if control {
            holders.insert(c.node.clone());
        }
        routes.set(&members(&a, &[&a.node, &b.node, &c.node]), &holders);
        let subject = format!("/f/{FLEET}/pub/{}/s-1/say/build", a.node);
        let mut bridge = a.client(a.key, &[format!("rw,p={}:{}", a.node, say_lanes(&a.node))]);
        let (at, _) = bridge
            .publish(pid(&a.node), 1, &subject, b"green")
            .expect("the bridge's broadcast lands");
        let said = |mac: &Mac| mac.holds(&say_lanes(&a.node));
        until(&format!("{case}: B holds A's broadcast"), || {
            !said(&b).is_empty()
        });
        let held = said(&b);
        assert_eq!(held.len(), 1, "{case}: once");
        let (_, on_b, body, origin) = &held[0];
        assert_eq!(on_b, &subject, "{case}");
        assert_eq!(body, b"green", "{case}");
        assert_eq!(*origin, Some((pid(&a.node), at)), "{case}");
        until(&format!("{case}: A's say group for B passes it"), || {
            a.broker.group_start(&say_group(&a.node, &b.node)) > at
        });
        if control {
            until(&format!("{case}: the leak reaches C"), || {
                !said(&c).is_empty()
            });
        } else {
            assert_eq!(
                pushes(&routes),
                [Lane::Say {
                    dst: b.node.clone()
                }],
                "{case}: one push, to B"
            );
            assert!(said(&c).is_empty(), "{case}: C holds nothing");
        }
        drop(routes);
    }
}

/// **UNREAD MAIL SURVIVES A PARTITION**: with B cut off, A's notes to B stay held on A through a
/// retention pass and a restart of A's pushes; once B is back they land there, each once, and
/// A's hold on them is released (its group for B, the mail rule's reader, passes them).
/// NEGATIVE CONTROL: [`ANOTHER_GROUP`] follows the push under a group of its own: the notes still
/// land, but the reader group never moves, so the sender holds them for good.
#[test]
fn unread_mail_survives_a_partition_and_is_released_once_carried() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (a, b) = (Mac::new(4), Mac::new(5));
        let net = Arc::new(Net::default());
        let handoff = members(&a, &[&a.node, &b.node]);
        ANOTHER_GROUP.with(|on| on.set(control));
        let routes = routes_of(&a, vec![Peer::of(&b)], &net, false);
        routes.set(&handoff, &BTreeSet::new());
        let first = a.post(&b.node, 1, b"before");
        until(&format!("{case}: the first note lands"), || {
            b.mail_for(&b.node).len() == 1
        });

        net.cut(&b.node);
        let during = [
            a.post(&b.node, 2, b"during 1"),
            a.post(&b.node, 3, b"during 2"),
        ];
        a.broker
            .retention_tick_at(a.broker.now_ms())
            .expect("a retention pass on A");
        drop(routes);
        let routes = routes_of(&a, vec![Peer::of(&b)], &net, false);
        ANOTHER_GROUP.with(|on| on.set(false));
        routes.set(&handoff, &BTreeSet::new());
        a.broker
            .retention_tick_at(a.broker.now_ms())
            .expect("another pass");
        let held: Vec<Vec<u8>> = a.mail_for(&b.node);
        assert!(
            held.contains(&b"during 1".to_vec()) && held.contains(&b"during 2".to_vec()),
            "{case}: A holds what B has not read: {held:?}"
        );
        assert_eq!(b.mail_for(&b.node), [b"before".to_vec()], "{case}");

        net.heal(&b.node);
        routes.kick();
        until(&format!("{case}: the held notes land"), || {
            b.mail_for(&b.node).len() == 3
        });
        assert_eq!(
            b.mail_for(&b.node),
            [
                b"before".to_vec(),
                b"during 1".to_vec(),
                b"during 2".to_vec()
            ],
            "{case}: each once, in order"
        );
        let released = || a.broker.group_start(&mail_group(&b.node)) > during[1];
        if control {
            assert!(
                a.broker.group_start(&mail_group(&b.node)) <= first,
                "{case}: the reader group never moved"
            );
        } else {
            until(&format!("{case}: A's hold is released"), released);
        }
        drop(routes);
    }
}

/// **HELD MAIL MOVES ACROSS A KEY ROTATION, ONCE**: A held notes for B while B was cut off; B
/// then rotated its node key (the log ties the old and new keys to one seat). A's push carries
/// each held note to B's new node under B's new subjects, once, and its group for the old node
/// passes them. A note A writes to the old node after the rotation goes the same way.
/// NEGATIVE CONTROL: a hand-off without the rotation runs no push for the old node, and nothing
/// reaches the new one.
#[test]
fn held_mail_moves_to_a_rotated_macs_new_node_once() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (a, b) = (Mac::new(6), Mac::new(7));
        let (new_node, new_key) = b.rekeyed(17);
        let new_b = Peer {
            node: new_node.clone(),
            ..Peer::of(&b)
        };
        let net = Arc::new(Net::default());
        net.cut(&b.node);
        let routes = routes_of(&a, vec![Peer::of(&b), new_b], &net, false);
        routes.set(&members(&a, &[&a.node, &b.node]), &BTreeSet::new());
        a.post(&b.node, 1, b"held 1");
        a.post(&b.node, 2, b"held 2");
        until(&format!("{case}: the watch saw the notes"), || {
            routes.written().contains(&b.node)
        });

        let mut rotated = members(&a, &[&a.node, &new_node]);
        if !control {
            rotated.moved.insert(b.node.clone(), new_node.clone());
        }
        routes.set(&rotated, &BTreeSet::new());
        let late = a.post(&b.node, 3, b"late");
        if control {
            assert!(
                pushes(&routes).is_empty(),
                "{case}: no push for a node that is no member"
            );
            assert!(b.mail_for(&new_node).is_empty(), "{case}");
            drop(routes);
            continue;
        }
        until(
            &format!("{case}: the held notes reach the new node"),
            || b.mail_for(&new_node).len() == 3,
        );
        let moved = b.holds(&format!("/f/{FLEET}/in/{new_node}/>"));
        assert_eq!(
            moved
                .iter()
                .map(|(_, subject, body, _)| (subject.clone(), body.clone()))
                .collect::<Vec<_>>(),
            [&b"held 1"[..], b"held 2", b"late"]
                .map(|body| (lane(&new_node, &a.node), body.to_vec()))
                .to_vec(),
            "{case}: under the new node's subjects, each once"
        );
        assert!(b.mail_for(&b.node).is_empty(), "{case}: none under the old");
        until(
            &format!("{case}: A's group for the old node passes them"),
            || a.broker.group_start(&mail_group(&b.node)) > late,
        );
        let _ = new_key;
        drop(routes);
    }
}

/// A's hand-off after A rotated from `a.node` to `new`: B and the new node the members, the old
/// node moved to the new one.
fn rotated_from(a: &Mac, new: &str, b: &Mac) -> Handoff {
    Handoff {
        me: new.to_owned(),
        members: [new.to_owned(), b.node.clone()].into(),
        moved: [(a.node.clone(), new.to_owned())].into(),
        rank: BTreeMap::new(),
        gone: BTreeSet::new(),
    }
}

/// **A RING NAMES THE NODES A SEAT RETIRED LAST** (§11 C3b): of more retired nodes than a ring
/// holds, [`ringed`] names the [`RING_OLDS_MAX`] the membership log ranks latest, whatever their
/// node order, and a node it does not rank after every one it does, in node order; a peer's ring
/// takes the mail its seat wrote under each of them ([`peer_ring`]), and no other.
/// NEGATIVE CONTROL: [`NODE_ORDER_RING`], the choice before the second recency phase: the first
/// in node order, which leave out the node retired last and name the one retired first.
#[test]
fn a_ring_names_the_nodes_a_seat_retired_last() {
    let (me, peer) = (node_of(&key_of(98)), node_of(&key_of(99)));
    let olds: BTreeSet<String> = (0..=RING_OLDS_MAX)
        .map(|i| node_of(&key_of(130 + u8::try_from(i).unwrap())))
        .collect();
    // Retired in node order: the greatest node last, which node order leaves out.
    let recent: Vec<String> = olds.iter().rev().cloned().collect();
    let rank: BTreeMap<String, usize> = recent.iter().cloned().zip(0..).collect();
    let (last, first) = (&recent[0], &recent[RING_OLDS_MAX]);
    let grant = |old: &str| format!("rw,p={peer}:/f/{FLEET}/in/{me}/*/{old}/*");
    for control in [false, true] {
        let case = format!("control={control}");
        NODE_ORDER_RING.with(|on| on.set(control));
        let named = ringed(&olds, &rank);
        NODE_ORDER_RING.with(|on| on.set(false));
        assert_eq!(named.len(), RING_OLDS_MAX, "{case}: bounded");
        assert_eq!(named.contains(last), !control, "{case}: the last retired");
        assert_eq!(named.contains(first), control, "{case}: the first retired");
        let ring = peer_ring(FLEET, &me, &peer, &named);
        for old in &olds {
            assert_eq!(
                ring.contains(&grant(old)),
                named.contains(old),
                "{case}: {old}"
            );
        }
    }
    let only_last: BTreeMap<String, usize> = [(last.clone(), 0)].into();
    let mut want: BTreeSet<String> = olds.iter().take(RING_OLDS_MAX - 1).cloned().collect();
    want.insert(last.clone());
    assert_eq!(
        ringed(&olds, &only_last),
        want,
        "the ranked first, then the rest in node order"
    );
}

/// **A MAC MOVES THE MAIL OF THE NODES ITS SEAT RETIRED LAST, AND SAYS WHICH IT DOES NOT** (§11
/// C3b): with one more retired node than a ring names, this Mac's own-inbox moves take the
/// [`RING_OLDS_MAX`] its seat retired last ([`Handoff::ringed`]), whatever their node order, the
/// ones the reader's ring the node service writes from the same hand-off reads
/// ([`write_old_reads`]); its pushes carry the ones a peer's ring grants
/// (`a_push_carries_the_retired_nodes_its_peers_ring_grants_whichever_they_are`); and the journal
/// names the one retired first, once.
/// NEGATIVE CONTROL: [`NODE_ORDER_RING`], the choice before the second recency phase: the moves
/// take the first in node order, which leave out the node retired last, and the journal names
/// that one; and with [`RING_OLDS_MAX`] retired nodes every one is taken and nothing said.
#[test]
fn a_mac_moves_the_mail_of_the_nodes_its_seat_retired_last_and_says_which_it_does_not() {
    let a = Mac::new(20);
    let net = Arc::new(Net::default());
    for (count, control) in [
        (RING_OLDS_MAX + 1, false),
        (RING_OLDS_MAX + 1, true),
        (RING_OLDS_MAX, false),
    ] {
        let case = format!("count={count} control={control}");
        let olds: BTreeSet<String> = (0..count)
            .map(|i| node_of(&key_of(100 + u8::try_from(i).unwrap())))
            .collect();
        // Retired in node order: the greatest node last, which node order leaves out.
        let rank: BTreeMap<String, usize> = olds.iter().rev().cloned().zip(0..).collect();
        let named: BTreeSet<String> = if control {
            olds.iter().take(RING_OLDS_MAX).cloned().collect()
        } else {
            olds.iter().rev().take(RING_OLDS_MAX).cloned().collect()
        };
        let left: Vec<&String> = olds.difference(&named).collect();
        let notes = Arc::new(Mutex::new(Vec::<String>::new()));
        let noted = Arc::clone(&notes);
        NODE_ORDER_RING.with(|on| on.set(control));
        let routes = routes_noting(
            &a,
            (&a.node, a.key),
            &olds,
            (Vec::new(), &net, false),
            Arc::new(move |line| noted.lock().unwrap().push(line.to_owned())),
            Arc::new(|_| true),
        );
        let handoff = Handoff {
            me: a.node.clone(),
            members: [a.node.clone()].into(),
            moved: olds
                .iter()
                .map(|old| (old.clone(), a.node.clone()))
                .collect(),
            rank,
            gone: BTreeSet::new(),
        };
        routes.set(&handoff, &BTreeSet::new());
        routes.set(&handoff, &BTreeSet::new());
        let reads = old_reads(&handoff.ringed(&a.node));
        NODE_ORDER_RING.with(|on| on.set(false));
        let moves: BTreeSet<String> = routes
            .status()
            .into_keys()
            .filter_map(|lane| match lane {
                Lane::Inbox { old } => Some(old),
                _ => None,
            })
            .collect();
        assert_eq!(moves, named, "{case}: the nodes the ring names, moved");
        assert_eq!(
            reads,
            named.iter().map(|old| old_read(old)).collect::<Vec<_>>(),
            "{case}: the reader's ring reads the nodes moved"
        );
        let said = notes.lock().unwrap().clone();
        if left.is_empty() {
            assert!(said.is_empty(), "{case}: {said:?}");
        } else {
            assert_eq!(said.len(), 1, "{case}: once: {said:?}");
            for old in &left {
                assert!(
                    said[0].contains(old.as_str()),
                    "{case}: names {old}: {said:?}"
                );
            }
            for old in &named {
                assert!(
                    !said[0].contains(old.as_str()),
                    "{case}: not {old}: {said:?}"
                );
            }
        }
        drop(routes);
    }
}

/// **A PUSH CARRIES THE RETIRED NODES ITS PEER'S RING GRANTS, WHICHEVER THEY ARE** (§11 C3b, the
/// first phase of choosing them by how recent each is): A's seat retired one more node than a ring
/// names, in node order, and B's ring names the [`RING_OLDS_MAX`] it retired last ([`ringed`]):
/// the last in node order, not the first. A's push to B carries the mail A wrote under the last one,
/// passes what it wrote under the first uncarried, journaling the first alone as not carried, once
/// across a reopen, and carries what it writes after.
/// NEGATIVE CONTROL: [`NODE_ORDER_OLDS`], each push carrying the first in node order as before:
/// it carries the first one's lane, which B's ring refuses, so every try fails there and A's group
/// for B never passes that record, the last one's lane, which B's ring grants, goes uncarried,
/// and the journal names no node as passed uncarried.
#[test]
fn a_push_carries_the_retired_nodes_its_peers_ring_grants_whichever_they_are() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (a, b) = (Mac::new(40), Mac::new(41));
        let tags: Vec<u8> = (0..=RING_OLDS_MAX)
            .map(|i| 120 + u8::try_from(i).unwrap())
            .collect();
        let olds: BTreeSet<String> = tags.iter().map(|tag| node_of(&key_of(*tag))).collect();
        let key_for = |old: &str| {
            let tag = tags
                .iter()
                .find(|tag| node_of(&key_of(**tag)) == old)
                .expect("a retired node's key");
            key_of(*tag)
        };
        let (first, last) = (
            olds.first().expect("nine").clone(),
            olds.last().expect("nine").clone(),
        );
        let rank: BTreeMap<String, usize> = olds.iter().rev().cloned().zip(0..).collect();
        let granted = ringed(&olds, &rank);
        assert_eq!(
            granted,
            olds.iter().skip(1).cloned().collect::<BTreeSet<_>>(),
            "{case}: all but the one retired first"
        );
        let net = Arc::new(Net::default());
        let notes = Arc::new(Mutex::new(Vec::<String>::new()));
        let noted = Arc::clone(&notes);
        NODE_ORDER_OLDS.with(|on| on.set(control));
        let routes = routes_noting(
            &a,
            (&a.node, a.key),
            &granted,
            (vec![Peer::of(&b)], &net, false),
            Arc::new(move |line| noted.lock().unwrap().push(line.to_owned())),
            Arc::new(|_| true),
        );
        NODE_ORDER_OLDS.with(|on| on.set(false));
        let passed = || -> Vec<String> {
            notes
                .lock()
                .unwrap()
                .iter()
                .filter(|line| line.contains("does not carry the lanes of"))
                .cloned()
                .collect()
        };
        let under = |old: &str, body: &[u8]| -> u64 {
            let mut client = a.client(
                key_for(old),
                &[format!("rw,p={old}:{}", written_lanes(old))],
            );
            client
                .publish(pid(old), 1, &lane(&b.node, old), body)
                .expect("A wrote under a retired node")
                .0
        };
        let refused = under(&first, b"under the first");
        under(&last, b"under the last");
        let handoff = Handoff {
            me: a.node.clone(),
            members: [a.node.clone(), b.node.clone()].into(),
            moved: olds
                .iter()
                .map(|old| (old.clone(), a.node.clone()))
                .collect(),
            rank: rank.clone(),
            gone: BTreeSet::new(),
        };
        routes.set(&handoff, &BTreeSet::new());
        let fresh = a.post(&b.node, 1, b"fresh");
        if control {
            let push = Lane::Mail {
                dst: b.node.clone(),
            };
            until(
                &format!("{case}: B's ring refuses the first one's lane"),
                || {
                    routes
                        .status()
                        .get(&push)
                        .is_some_and(|status| status.last_error.is_some())
                },
            );
            assert!(
                a.broker.group_start(&mail_group(&b.node)) <= refused,
                "{case}: A's group for B never passes the refused record"
            );
            assert!(
                !b.mail_for(&b.node).contains(&b"under the last".to_vec()),
                "{case}: the lane B's ring grants goes uncarried"
            );
            assert_eq!(passed(), Vec::<String>::new(), "{case}: nothing named");
        } else {
            until(&format!("{case}: the fresh note lands"), || {
                b.mail_for(&b.node).contains(&b"fresh".to_vec())
            });
            let landed = b.mail_for(&b.node);
            assert!(
                landed.contains(&b"under the last".to_vec()),
                "{case}: the lane B's ring grants is carried: {landed:?}"
            );
            assert!(
                !landed.contains(&b"under the first".to_vec()),
                "{case}: the one it does not is not: {landed:?}"
            );
            until(&format!("{case}: A's group for B passes them all"), || {
                a.broker.group_start(&mail_group(&b.node)) > fresh
            });
            let said = passed();
            assert_eq!(said.len(), 1, "{case}: once: {said:?}");
            assert!(
                said[0].contains(first.as_str()) && !said[0].contains(last.as_str()),
                "{case}: names the first alone: {said:?}"
            );
            let push = Lane::Mail {
                dst: b.node.clone(),
            };
            net.cut(&b.node);
            a.post(&b.node, 2, b"after the reopen");
            until(&format!("{case}: the cut ends the push"), || {
                routes
                    .status()
                    .get(&push)
                    .is_some_and(|status| status.last_error.is_some())
            });
            net.heal(&b.node);
            routes.kick();
            until(&format!("{case}: the reopened push carries"), || {
                b.mail_for(&b.node).contains(&b"after the reopen".to_vec())
            });
            assert_eq!(passed(), said, "{case}: once across the reopen");
        }
        drop(routes);
    }
}

/// **A ROTATED SENDER CARRIES WHAT IT HELD UNDER ITS OLD NODE, ONCE** (§11 C3b): A held notes
/// for B while B was cut off, then rotated its node key, on the same `bus.log`. Its push to B,
/// now under its new node and producer, carries those notes under their subjects (B's ring takes
/// A's mail under the node its seat held before) and a note A writes after, each once and in
/// order, once B is back; A's group for B, the mail rule's reader of every one of those lanes,
/// passes all of them.
/// NEGATIVE CONTROL: [`ONE_NODE`], the pushes before C3b, counts and carries the new node's lanes
/// alone: the late note lands and the held ones never do, while the group that held them passes
/// them unsent.
#[test]
fn a_rotated_sender_carries_the_mail_it_held_under_its_old_node_once() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (a, b) = (Mac::new(8), Mac::new(9));
        let (new_node, new_key) = a.rekeyed(18);
        let net = Arc::new(Net::default());
        net.cut(&b.node);
        let routes = routes_of(&a, vec![Peer::of(&b)], &net, false);
        routes.set(&members(&a, &[&a.node, &b.node]), &BTreeSet::new());
        let held = [a.post(&b.node, 1, b"held 1"), a.post(&b.node, 2, b"held 2")];
        until(&format!("{case}: the watch saw the notes"), || {
            routes.written().contains(&b.node)
        });
        drop(routes);

        // A's node key rotates: the node service's next generation, on the same bus.log.
        let olds = BTreeSet::from([a.node.clone()]);
        ONE_NODE.with(|on| on.set(control));
        let routes = routes_as(
            &a,
            (&new_node, new_key),
            &olds,
            vec![Peer::of(&b)],
            &net,
            false,
        );
        ONE_NODE.with(|on| on.set(false));
        routes.set(&rotated_from(&a, &new_node, &b), &BTreeSet::new());
        net.heal(&b.node);
        routes.kick();
        let mut client = a.client(
            new_key,
            &[format!("rw,p={new_node}:{}", written_lanes(&new_node))],
        );
        let late = client
            .publish(pid(&new_node), 3, &lane(&b.node, &new_node), b"late")
            .expect("the new node's bridge posts")
            .0;
        until(&format!("{case}: the late note lands"), || {
            b.mail_for(&b.node).contains(&b"late".to_vec())
        });
        until(
            &format!("{case}: A's group for B passes the late note"),
            || a.broker.group_start(&mail_group(&b.node)) > late,
        );
        let landed: Vec<(String, Vec<u8>)> = b
            .holds(&format!("/f/{FLEET}/in/{}/>", b.node))
            .into_iter()
            .map(|(_, subject, body, _)| (subject, body))
            .collect();
        if control {
            assert_eq!(
                landed,
                [(lane(&b.node, &new_node), b"late".to_vec())],
                "{case}: the held notes are never carried"
            );
            assert!(
                a.broker.group_start(&mail_group(&b.node)) > held[1],
                "{case}: and the group that held them has passed them"
            );
        } else {
            assert_eq!(
                landed,
                [
                    (lane(&b.node, &a.node), b"held 1".to_vec()),
                    (lane(&b.node, &a.node), b"held 2".to_vec()),
                    (lane(&b.node, &new_node), b"late".to_vec()),
                ],
                "{case}: each once, in order, under the subjects A wrote"
            );
        }
        drop(routes);
    }
}

/// **A BURST THE OLD NODE LANDED AND NEVER COMMITTED LANDS ONCE**: A's push under its old node
/// had B take its held notes when the generation ended, before A's group for B moved past them
/// (a stop mid-burst). The new node's push sends them again under its new producer, and B, told
/// the new node continues the old one's sequence (as aterm-mesh's `lineage` tells `bus.log` from
/// the standings), answers them from the records it holds: each lands once.
/// NEGATIVE CONTROL: B told nothing appends each a second time.
#[test]
fn a_burst_the_old_node_landed_and_never_committed_lands_once() {
    for control in [false, true] {
        let case = format!("control={control}");
        let (a, b) = (Mac::new(10), Mac::new(11));
        let (new_node, new_key) = a.rekeyed(20);
        let held = [a.post(&b.node, 1, b"held 1"), a.post(&b.node, 2, b"held 2")];
        // What the old push left on B: its burst appended, numbered by A's offsets.
        let mut old_push = b.client(
            a.key,
            &peer_ring(FLEET, &b.node, &a.node, &BTreeSet::new())[..1],
        );
        for (off, body) in held.iter().zip([&b"held 1"[..], b"held 2"]) {
            old_push
                .publish(pid(&a.node), *off, &lane(&b.node, &a.node), body)
                .expect("the old push's burst lands on B");
        }
        assert_eq!(a.broker.group_start(&mail_group(&b.node)), 0, "{case}");
        if !control {
            b.broker
                .set_producer_lineage([(pid(&new_node), vec![pid(&a.node)])]);
        }

        let net = Arc::new(Net::default());
        let olds = BTreeSet::from([a.node.clone()]);
        let routes = routes_as(
            &a,
            (&new_node, new_key),
            &olds,
            vec![Peer::of(&b)],
            &net,
            false,
        );
        routes.set(&rotated_from(&a, &new_node, &b), &BTreeSet::new());
        // The push reports a burst after it commits A's group for it, so the wait reads both.
        let lane = Lane::Mail {
            dst: b.node.clone(),
        };
        until(&format!("{case}: A's group for B passes the burst"), || {
            a.broker.group_start(&mail_group(&b.node)) > held[1]
                && routes
                    .status()
                    .get(&lane)
                    .is_some_and(|pushed| pushed.appended + pushed.deduped >= 2)
        });
        let want: Vec<Vec<u8>> = if control {
            vec![
                b"held 1".to_vec(),
                b"held 2".to_vec(),
                b"held 1".to_vec(),
                b"held 2".to_vec(),
            ]
        } else {
            vec![b"held 1".to_vec(), b"held 2".to_vec()]
        };
        assert_eq!(b.mail_for(&b.node), want, "{case}");
        let status = routes.status();
        let pushed = &status[&lane];
        assert_eq!(
            (pushed.appended, pushed.deduped),
            if control { (2, 0) } else { (0, 2) },
            "{case}"
        );
        drop(routes);
    }
}
