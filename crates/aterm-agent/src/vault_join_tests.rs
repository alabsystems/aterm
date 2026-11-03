// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_net::fleet_member::{Standing, mint_secret};

fn mark() -> FleetMark {
    FleetSecret::new(&mint_secret().unwrap()).unwrap().mark()
}

fn proof(key: u8) -> NodeProof {
    NodeProof {
        node: format!("{key:032x}"),
        key: NodeKey::from_bytes([key; 32]),
    }
}

/// What a `vault-join` channel is worth: none without a node proof (an `atf/1` dialer that sent
/// `node -`), the fleet's key only where the secret it proved is the vault's current fleet code
/// and this Mac's drive admission takes the node (a Mac removed without `--stolen` holds the old
/// code, which the removal rotated), and a node id the book binds to another key the log holds
/// out, or seats, is named so `decide` refuses it.
///
/// NEGATIVE CONTROLS: `judge` reading the fleet's key without the code check (`fleet: driven`):
/// the channel that proved the old code counts as the fleet's and the "stale code" assert fails.
/// `node_id_seated` never set (`log.admits(&key) && false`): "the book binds the node id to
/// another seat's key" fails.
#[test]
fn a_vault_join_counts_the_fleets_key_only_for_the_current_code_and_a_driver() {
    let (now, old) = (mark(), mark());
    assert!(
        judge(
            None,
            (now, Some(now)),
            true,
            (None, &[], None),
            (Some(&[]), None, 0)
        )
        .is_err(),
        "a channel with no node proof"
    );
    let p = proof(7);
    let judged = judge(
        Some(&p),
        (now, Some(now)),
        true,
        (None, &[], None),
        (Some(&[]), None, 0),
    )
    .unwrap();
    assert_eq!(
        judged,
        Judged {
            node: [7; 32],
            fleet: true,
            book: Book {
                node_id_held_out: false,
                node_id_seated: false,
                node_elsewhere: false,
                moved_seats_read: true,
            }
        }
    );
    assert!(
        !judge(
            Some(&p),
            (old, Some(now)),
            true,
            (None, &[], None),
            (Some(&[]), None, 0)
        )
        .unwrap()
        .fleet,
        "stale code: a removed Mac's copy of the fleet code is no proof of the fleet's key"
    );
    assert!(
        !judge(
            Some(&p),
            (now, None),
            true,
            (None, &[], None),
            (Some(&[]), None, 0)
        )
        .unwrap()
        .fleet,
        "a vault with no fleet code"
    );
    assert!(
        !judge(
            Some(&p),
            (now, Some(now)),
            false,
            (None, &[], None),
            (Some(&[]), None, 0)
        )
        .unwrap()
        .fleet,
        "a node this Mac's drive admission refuses"
    );
    let removed = NodeKey::from_bytes([9; 32]);
    let log: MemberKeys = [(removed, Standing::Out("removed".to_owned()))]
        .into_iter()
        .collect();
    assert!(
        judge(
            Some(&p),
            (now, Some(now)),
            true,
            (Some(removed), &[], Some(&log)),
            (Some(&[]), None, 0)
        )
        .unwrap()
        .book
        .node_id_held_out,
        "the book binds the node id to a key the log took out"
    );
    let seated = NodeKey::from_bytes([10; 32]);
    let log_seated: MemberKeys = [(seated, Standing::Member)].into_iter().collect();
    let judged = judge(
        Some(&p),
        (now, Some(now)),
        true,
        (Some(seated), &[], Some(&log_seated)),
        (Some(&[]), None, 0),
    )
    .unwrap();
    assert!(
        judged.book.node_id_seated && !judged.book.node_id_held_out,
        "the book binds the node id to another seat's key"
    );
    assert!(
        !judge(
            Some(&p),
            (now, Some(now)),
            true,
            (Some(p.key), &[], Some(&log_seated)),
            (Some(&[]), None, 0),
        )
        .unwrap()
        .book
        .node_id_seated,
        "the node id's own key"
    );
    assert!(
        judge(
            Some(&p),
            (now, Some(now)),
            true,
            (Some(removed), std::slice::from_ref(&p.node), Some(&log)),
            (Some(&[]), None, 0)
        )
        .is_err(),
        "a node on the deny rows"
    );
}

fn member(version: Option<&str>, seen_ms: Option<u64>) -> Peer {
    Peer {
        node: Some("0123456789abcdef0123456789abcdef".to_owned()),
        key: None,
        key_from: None,
        host: "m9.local".to_owned(),
        addr: None,
        port: 7100,
        version: version.map(str::to_owned),
        seen_ms,
        via: aterm_net::fleet_member::Via::Joined,
        fleet: None,
        signed: None,
    }
}

/// A seat moves by proof only while every member the book heard in the last two weeks runs
/// 0.148.0 or later (`moved_seats_read`): a 0.147 member, a dev build of 0.147, or an entry that
/// names no version (an older aterm's) heard within the window keeps every seat where it is; one
/// last heard past the window, or a peer a person named, does not count.
///
/// NEGATIVE CONTROLS: `reads_moved_seats` comparing the major number alone: the 0.147 member
/// passes and the "a 0.147 member" assert fails. The window filter dropped from
/// `moved_seats_read`: the member asleep three weeks counts and the "long asleep" assert fails.
#[test]
fn a_seat_moves_only_once_every_member_the_book_heard_reads_moved_seats() {
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let now = 1_000 * DAY;
    let current = member(Some("0.148.0+g05a95cb4a5"), Some(now - DAY));
    assert!(moved_seats_read(std::slice::from_ref(&current), now));
    assert!(moved_seats_read(&[member(Some("1.2.0"), Some(now))], now));
    assert!(
        !moved_seats_read(
            &[current.clone(), member(Some("0.147.0"), Some(now - DAY))],
            now
        ),
        "a 0.147 member"
    );
    assert!(
        !moved_seats_read(&[member(Some("0.147.0+g0123abc"), Some(now))], now),
        "a dev build of 0.147"
    );
    assert!(
        !moved_seats_read(&[member(None, Some(now))], now),
        "an entry that names no version"
    );
    assert!(
        moved_seats_read(
            &[
                current.clone(),
                member(Some("0.147.0"), Some(now - 21 * DAY))
            ],
            now
        ),
        "long asleep"
    );
    let named = Peer {
        node: None,
        ..member(Some("0.146.0"), Some(now))
    };
    assert!(
        moved_seats_read(&[current, named], now),
        "a peer a person named"
    );
    assert!(
        !book_word(&[member(Some("0.147.0"), Some(now))], &[7; 32], None, now).moved_seats_read
    );
    for malformed in [
        "0+148",
        "0-148",
        "0.148",
        "0.148.0.1",
        "00.148.0",
        "0.0148.0",
        "0.148.0+",
        "0.148.0+bad space",
    ] {
        assert!(
            !moved_seats_read(&[member(Some(malformed), Some(now))], now),
            "malformed version counted as reading moved seats: {malformed:?}"
        );
    }
}

/// The book hears a node key elsewhere (`node_elsewhere`) only from a live entry of that very key
/// at another address than the proof's: the Mac's own heartbeat from where it proves itself, a
/// stale entry, an entry of another key and one with no address say nothing; an unknown source
/// address makes any live address another.
///
/// NEGATIVE CONTROL: the address comparison dropped (`from.is_none_or(|_| true)`): the Mac's own
/// live entry counts and the "its own heartbeat" assert fails.
#[test]
fn a_node_key_is_heard_elsewhere_only_from_a_live_entry_at_another_address() {
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let now = 1_000 * DAY;
    let key = [7; 32];
    let at = |addr: [u8; 4], seen: u64, key: [u8; 32]| Peer {
        key: Some(NodeKey::from_bytes(key)),
        addr: Some(IpAddr::from(addr)),
        ..member(Some("0.148.0"), Some(seen))
    };
    let here = IpAddr::from([192, 168, 1, 40]);
    assert!(
        !node_elsewhere(
            &[at([192, 168, 1, 40], now - 60_000, key)],
            &key,
            Some(here),
            now
        ),
        "its own heartbeat"
    );
    assert!(node_elsewhere(
        &[at([192, 168, 1, 41], now - 60_000, key)],
        &key,
        Some(here),
        now
    ));
    assert!(
        !node_elsewhere(
            &[at([192, 168, 1, 41], now - DAY, key)],
            &key,
            Some(here),
            now
        ),
        "a stale entry"
    );
    assert!(
        !node_elsewhere(
            &[at([192, 168, 1, 41], now, [8; 32])],
            &key,
            Some(here),
            now
        ),
        "another key"
    );
    let no_addr = Peer {
        addr: None,
        ..at([192, 168, 1, 41], now, key)
    };
    assert!(
        !node_elsewhere(&[no_addr], &key, Some(here), now),
        "no address"
    );
    assert!(
        node_elsewhere(&[at([192, 168, 1, 40], now, key)], &key, None, now),
        "an unknown source"
    );
}

/// A synthetic member `name` under `root`'s desk identity (nothing touches a real vault).
fn local(root: &std::path::Path, name: &str) -> Local {
    use aterm_secrets::commons::{bootstrap::Device, journal::Trust};
    let paths = Paths::for_home(&root.join(name));
    client::machine_identity(&paths).expect("identity");
    let device = Device::open(&paths).expect("device").expect("an identity");
    let trust = Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    };
    Local::with(device, trust, true, name).expect("member")
}

/// A Mac with no window serves every `vault-join` on one desk (`headless_desk`), so the desk's
/// limit of runs one source starts a minute holds across connections there too; a changed member
/// key gets a desk of its own.
///
/// NEGATIVE CONTROL: `headless_desk` making a desk for each call (its `filter` answering `false`):
/// the second connection gets a fresh desk and the "one desk across connections" assert fails.
#[test]
fn a_mac_with_no_window_keeps_one_desk_across_vault_joins() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let first = headless_desk(local(root.path(), "m1"));
    let again = headless_desk(local(root.path(), "m1"));
    assert!(Arc::ptr_eq(&first, &again), "one desk across connections");
    let refreshed =
        headless_desk(local(root.path(), "m1").with_automatic_lan(std::iter::empty::<String>()));
    assert!(Arc::ptr_eq(&first, &refreshed));
    let peer = "192.168.1.40".parse().unwrap();
    assert!(
        refreshed
            .lock_or_recover()
            .local()
            .automatically_admits(peer)
    );
    let moved = headless_desk(
        local(root.path(), "m1")
            .with_automatic_lan(std::iter::empty::<String>())
            .with_home(None),
    );
    assert!(Arc::ptr_eq(&first, &moved));
    assert!(!moved.lock_or_recover().local().automatically_admits(peer));
    let other = headless_desk(local(root.path(), "m2"));
    assert!(
        !Arc::ptr_eq(&first, &other),
        "another member key, another desk"
    );
}

/// How long the loopback `vault-join` test waits on anything that must happen: a hang detector.
const HANG: Duration = Duration::from_secs(60);

/// What the desk behind one loopback `vault-join` listener was asked to decide: whether the
/// proof counted as the fleet's, and the node key it named.
type Seen = Arc<Mutex<Vec<(bool, [u8; 32])>>>;

/// A member's `vault-join` listener on loopback (`atf/2`, node `m22`), proving `listening` as its
/// fleet secret while the vault's current fleet code is `vault`: it judges each channel as the
/// window does ([`judge`], this Mac's drive admission taking every node) and serves it with
/// [`serve_deciding`] on one desk, whose decision notes what it was asked and refuses (the
/// re-admission itself is `client::readmit`'s, tested there on real vault folders).
struct Member {
    port: u16,
    running: Arc<std::sync::atomic::AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
    seen: Seen,
}

impl Member {
    fn listen(root: &std::path::Path, listening: &FleetSecret, vault: FleetMark) -> Self {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let desk = Arc::new(Mutex::new(Desk::new(local(root, "m22"))));
        let paths = Paths::for_home(&root.join("m22"));
        let seen: Seen = Arc::default();
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let config = aterm_net::fleet::Listening {
            secret: listening.clone(),
            scope: aterm_net::fleet::FleetScope::Facts,
            node: NodeIdentity::from_seed(&"22".repeat(16), &[22; 32]),
            serves: &[FleetOp::VaultJoin],
            slots: 4,
            per_source: 4,
            lifetime: HANG,
        };
        let mark = listening.mark();
        let hand_over = {
            let seen = Arc::clone(&seen);
            move |served: aterm_net::fleet::Served, _: FleetOp| {
                let from = served.peer_addr().map(|at| at.ip());
                let judged = judge(
                    served.peer_node(),
                    (mark, Some(vault)),
                    true,
                    (None, &[], None),
                    (Some(&[]), from, 0),
                );
                let (desk, paths, seen) = (Arc::clone(&desk), paths.clone(), Arc::clone(&seen));
                served.answer(move |stream| {
                    stream.set_read_timeout(Some(HANG))?;
                    stream.set_write_timeout(Some(HANG))?;
                    let decide = |proving: &desk::Proving<'_>| {
                        seen.lock_or_recover().push((proving.fleet, proving.node));
                        Err(aterm_secrets::Error::new("decided by the test's member"))
                    };
                    match judged {
                        Ok(judged) => {
                            let peer = from.unwrap_or(std::net::Ipv4Addr::LOCALHOST.into());
                            drop(serve_deciding(
                                stream,
                                peer,
                                &judged,
                                Some(&desk),
                                &paths,
                                (&decide, &|_| None),
                            ));
                        }
                        Err(why) => drop(aterm_secrets::members::refuse(stream, &why)),
                    }
                    Ok(())
                })
            }
        };
        let serving = std::thread::spawn({
            let running = Arc::clone(&running);
            move || {
                aterm_net::fleet::serve_with_handshake(
                    &listener,
                    &config,
                    &running,
                    HANG,
                    hand_over,
                    |_| {},
                );
            }
        });
        Self {
            port,
            running,
            serving: Some(serving),
            seen,
        }
    }

    fn seen(&self) -> Vec<(bool, [u8; 32])> {
        self.seen.lock_or_recover().clone()
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Release);
        drop(std::net::TcpStream::connect((
            std::net::Ipv4Addr::LOCALHOST,
            self.port,
        )));
        if let Some(serving) = self.serving.take() {
            drop(serving.join());
        }
    }
}

/// The fleet's own channel end to end on loopback: a Mac proving its node key over `vault-join`
/// ([`prove_over_fleet`], a real `atf/2` handshake) reaches the member's desk, whose decision
/// is asked with the node key the channel proved, counted as the fleet's only while the secret
/// the channel proved is the vault's current fleet code. A dialer holding another fleet's secret
/// never reaches the desk, nor does a channel that proved no node (`node -`), which reads the
/// listener's refusal.
///
/// NEGATIVE CONTROL: `serve_deciding` handing the desk the channel's node as the fleet's
/// whatever `judge` said (`fleet: Some(judged.node)`): the channel that proved a stale code is
/// asked as the fleet's and the "stale code" assert fails.
#[test]
fn a_vault_join_over_the_fleets_channel_reaches_the_desk_only_as_the_channel_proved() {
    use aterm_secrets::commons::bootstrap::Device;
    let root = aterm_tempfile::tempdir().expect("scratch");
    let fresh = Paths::for_home(&root.path().join("m7-fresh"));
    client::machine_identity(&fresh).expect("identity");
    let device = Device::open(&fresh).expect("device").expect("an identity");
    let m7 = NodeIdentity::from_seed(&"07".repeat(16), &[7; 32]).expect("node");
    let joining = Joining {
        device: &device,
        node: *m7.key().bytes(),
        name: "m7".to_owned(),
        model: "Mac15,3".to_owned(),
    };
    let secret = |_: ()| FleetSecret::new(&mint_secret().unwrap()).unwrap();
    let (current, stale, foreign) = (secret(()), secret(()), secret(()));
    let at = |member: &Member| {
        vec![(
            "127.0.0.1".to_owned(),
            Some(std::net::Ipv4Addr::LOCALHOST.into()),
            member.port,
        )]
    };
    let refusals = |missed: join::Missed| -> String {
        missed
            .refused
            .iter()
            .map(|(_, why)| why.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    };

    // The vault's current code: the desk is asked with the proved node, as the fleet's.
    let member = Member::listen(root.path(), &current, current.mark());
    let missed = prove_over_fleet(&fresh, &joining, (&current, &m7), &at(&member), None)
        .expect_err("the test's member refuses");
    assert!(
        refusals(missed).contains("decided by the test's member"),
        "the proof reached the desk's decision"
    );
    assert_eq!(member.seen(), [(true, *m7.key().bytes())]);
    drop(member);

    // A removed Mac's copy of the code the removal rotated: asked, but not as the fleet's, and
    // still bound to the node key its channel proved so one channel cannot name another Mac's
    // node key.
    let member = Member::listen(root.path(), &stale, current.mark());
    let m8 = NodeIdentity::from_seed(&"08".repeat(16), &[8; 32]).expect("node");
    let dial_m8 = |_: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
        let reached = aterm_net::fleet_member::dial(
            "127.0.0.1",
            Some(std::net::Ipv4Addr::LOCALHOST.into()),
            member.port,
            (&stale, Some(&m8)),
            FleetOp::VaultJoin,
            HANG,
        )
        .map_err(|error| aterm_secrets::Error::new(error.to_string()))?;
        let stream = reached.transport.into_stream();
        stream.sock.set_read_timeout(Some(HANG))?;
        stream.sock.set_write_timeout(Some(HANG))?;
        Ok(Box::new(stream))
    };
    let sign_m7 = |message: &[u8]| -> [u8; 64] { m7.sign_record(desk::PROVE_RECORD, message) };
    let mismatched = join::prove_first(
        &fresh,
        &dial_m8,
        &joining,
        &sign_m7,
        &[format!("127.0.0.1:{}", member.port)],
    )
    .expect_err("a channel proved as m8 cannot submit m7's proof");
    assert!(
        refusals(mismatched).contains("another node key than the fleet's channel proved"),
        "channel-binding refusal under stale code"
    );
    assert!(
        member.seen().is_empty(),
        "a mismatched node key under a stale fleet code never reaches decide"
    );
    let missed = prove_over_fleet(&fresh, &joining, (&stale, &m7), &at(&member), None)
        .expect_err("the test's member refuses");
    assert!(refusals(missed).contains("decided by the test's member"));
    assert_eq!(
        member.seen(),
        [(false, *m7.key().bytes())],
        "stale code: the channel's proof is not the fleet's"
    );
    drop(member);

    // Another fleet's secret: no channel, so no desk.
    let member = Member::listen(root.path(), &current, current.mark());
    let missed = prove_over_fleet(&fresh, &joining, (&foreign, &m7), &at(&member), None)
        .expect_err("a wrong secret reaches nothing");
    assert!(
        !refusals(missed).contains("decided by the test's member"),
        "a wrong secret"
    );
    assert!(member.seen().is_empty(), "a wrong secret reaches no desk");

    // A channel that proved no node: the listener refuses it before any desk.
    let reached = aterm_net::fleet_member::dial(
        "127.0.0.1",
        Some(std::net::Ipv4Addr::LOCALHOST.into()),
        member.port,
        (&current, None),
        FleetOp::VaultJoin,
        HANG,
    )
    .expect("a fleet channel with no node proof");
    let mut stream = reached.transport.into_stream();
    stream.sock.set_read_timeout(Some(HANG)).unwrap();
    let mut said = String::new();
    std::io::Read::read_to_string(&mut stream, &mut said).expect("the listener's answer");
    assert!(
        said.starts_with("NO ") && said.contains("proved%20its%20node%20key"),
        "node -: the listener says why: {said:?}"
    );
    assert!(member.seen().is_empty(), "node -: no desk");
}

/// A fleet book that could not be read lets no seat move; one read and empty has no older member.
/// NEGATIVE CONTROL: an unread book taken as an empty list: `moved_seats_read` is true and this fails.
#[test]
fn an_unread_book_lets_no_seat_move() {
    assert_eq!(book_word_read(None, &[7; 32], None, 1), Book::default());
    assert!(book_word_read(Some(&[]), &[7; 32], None, 1).moved_seats_read);
}
