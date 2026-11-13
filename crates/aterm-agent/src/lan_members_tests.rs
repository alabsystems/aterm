// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_net::fleet::{FleetOp, FleetScope, FleetSecret, NodeIdentity};

const HANG: Duration = Duration::from_secs(60);

#[test]
fn ineligible_addresses_do_not_consume_the_physical_link_budget() {
    let mut addresses: Vec<IpAddr> = (1..=MAX_LINKS as u8)
        .map(|last| Ipv4Addr::new(10, 0, 0, last).into())
        .collect();
    addresses.push(Ipv4Addr::LOCALHOST.into());
    let selected = source_addresses(addresses);
    let bound = links(&selected, 0, Scope::Loopback);
    assert_eq!(bound.len(), 1);
    assert_eq!(bound[0].0, Ipv4Addr::LOCALHOST);
}

#[test]
fn reply_budget_allows_concurrent_joiners_and_bounded_packet_loss_recovery() {
    let now = Instant::now();
    let source: IpAddr = Ipv4Addr::LOCALHOST.into();
    let first = request(&[1; NONCE]);
    let second = request(&[2; NONCE]);
    let mut window = ReplyWindow::new(now);
    assert!(window.admit(source, &first, now));
    assert!(
        window.admit(source, &second, now),
        "init and subscriber can ask together"
    );
    assert!(!window.admit(source, &first, now), "duplicate flood");
    assert!(
        window.admit(source, &first, now + RETRANSMIT),
        "lost response"
    );
    assert!(window.admit(source, &second, now + RETRANSMIT));
    assert!(
        !window.admit(source, &first, now + RETRANSMIT * 2),
        "per-source ceiling"
    );
    for last in 1..=MAX_PEERS - 4 {
        assert!(window.admit(Ipv4Addr::new(10, 0, 0, last as u8).into(), &first, now));
    }
    assert!(
        !window.admit(Ipv4Addr::new(10, 0, 1, 1).into(), &first, now),
        "global ceiling"
    );
    assert!(window.admit(source, &first, now + REFRESH));
}

#[test]
fn packets_are_bounded_versioned_and_bound_to_this_solicitation() {
    let nonce = [7; NONCE];
    let source: SocketAddr = "127.0.0.1:41385".parse().unwrap();
    let key = NodeKey::from_bytes([9; 32]);
    let ask = request(&nonce);
    let reply = response(&ask, 7100, key).unwrap();
    assert_eq!(
        hint(&reply, source, &nonce),
        Some(Hint {
            address: "127.0.0.1:7100".parse().unwrap(),
            key,
        })
    );
    assert!(
        hint(&reply, source, &[8; NONCE]).is_none(),
        "old or unsolicited reply"
    );
    assert!(hint(&reply[..RESPONSE - 1], source, &nonce).is_none());
    let mut extended = reply.to_vec();
    extended.push(0);
    assert!(hint(&extended, source, &nonce).is_none());
    assert!(response(&ask, 0, key).is_none());
    assert!(
        response(&reply, 7100, key).is_none(),
        "responses never elicit responses"
    );
    let mut unknown = ask;
    unknown[0] ^= 1;
    assert!(response(&unknown, 7100, key).is_none());

    let ready = announcement(&nonce, 1);
    assert!(ready_token(&ready).is_some());
    assert!(ready_token(&ready[..READY - 1]).is_none());
    let mut extended = ready.to_vec();
    extended.push(0);
    assert!(ready_token(&extended).is_none());
    assert!(ready_token(&ask).is_none());
    assert!(ready_token(&reply).is_none());
    assert!(hint(&ready, source, &nonce).is_none());
    assert!(response(&ready, 7100, key).is_none());
}

/// A later reply carries the original nonce. Starting another solicitation cannot
/// recover it, and no DNS or membership authority is involved in receiving it.
#[test]
fn a_native_solicitation_keeps_later_replies_after_its_first_batch() {
    let member = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    let client = links(&[Ipv4Addr::LOCALHOST], member.port(), Scope::Loopback);
    let mut probe = Probe::on(client, HANG).unwrap();
    let until = Instant::now() + HANG;
    let ask = loop {
        aterm_uds::ready::any_until(&[member.as_fd()], until).unwrap();
        if let Some((wire, _)) = member.receive().unwrap()
            && wire.len() == REQUEST
        {
            break wire;
        }
        assert!(Instant::now() < until, "a native request must arrive");
    };
    member
        .send(&response(&ask, 7101, NodeKey::from_bytes([1; 32])).unwrap())
        .unwrap();
    let first = probe.next(until).unwrap();
    assert_eq!(
        first
            .iter()
            .map(|hint| hint.address.port())
            .collect::<Vec<_>>(),
        [7101]
    );
    // Publish only after the consumer took its first batch. This models a second
    // member's reply while the caller authenticates the first, without a timing race.
    member
        .send(&response(&ask, 7102, NodeKey::from_bytes([2; 32])).unwrap())
        .unwrap();
    let next = probe.next(until).unwrap();
    assert_eq!(
        next.iter()
            .map(|hint| hint.address.port())
            .collect::<Vec<_>>(),
        [7102]
    );
}

#[test]
fn native_reply_continuation_rejects_duplicates_foreign_nonces_and_expired_proof() {
    let (mut probe, reply) = queued_test_probe();
    let until = Instant::now() + HANG;
    reply(7101, NodeKey::from_bytes([1; 32]));
    assert_eq!(probe.next(until).unwrap().len(), 1);
    reply(7101, NodeKey::from_bytes([9; 32]));
    let sender = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        GROUP,
        probe.active[0].1.port(),
        Scope::Loopback,
    )
    .unwrap();
    let mut foreign = probe.nonce;
    foreign[0] ^= 1;
    sender
        .send(&response(&request(&foreign), 7199, NodeKey::from_bytes([9; 32])).unwrap())
        .unwrap();
    sender.send(b"malformed").unwrap();
    reply(7102, NodeKey::from_bytes([2; 32]));
    let later = probe.next(until).unwrap();
    assert_eq!(
        later
            .iter()
            .map(|hint| hint.address.port())
            .collect::<Vec<_>>(),
        [7102]
    );
    reply(7103, NodeKey::from_bytes([3; 32]));
    assert!(
        probe.next(Instant::now()).unwrap().is_empty(),
        "a queued reply cannot renew the proof deadline"
    );
}

#[test]
fn native_reply_cardinality_is_global_across_batches() {
    let (mut probe, reply) = queued_test_probe();
    let until = Instant::now() + HANG;
    reply(7200, NodeKey::from_bytes([1; 32]));
    assert_eq!(probe.next(until).unwrap().len(), 1);
    for offset in 1..=MAX_PEERS {
        reply(7200 + offset as u16, NodeKey::from_bytes([2; 32]));
    }
    let rest = probe.complete(until).unwrap();
    assert_eq!(rest.len(), MAX_PEERS - 1);
    assert_eq!(probe.seen.len(), MAX_PEERS);
    assert!(probe.next(until).unwrap().is_empty());
}

#[test]
fn native_solicitation_reaches_the_member_handshake_without_dns() {
    let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    let discovery_port = reservation.port();
    let mut arrivals = Arrivals::on(
        Network {
            scope: Scope::Loopback,
            port: discovery_port,
            sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
            ready: None,
        },
        None,
    )
    .unwrap();
    // A real bounded solicitation first misses: no responder exists yet. The receiver
    // remains owned after that probe, and a later bind must wake it through the wire.
    let mut missed = Probe::on(
        links(&[Ipv4Addr::LOCALHOST], discovery_port, Scope::Loopback),
        RETRANSMIT,
    )
    .unwrap();
    assert!(missed.first().unwrap().is_empty());
    drop(missed);
    assert!(!arrivals.poll().unwrap());
    let identity = |seed| {
        let provisional =
            NodeIdentity::from_seed("00000000000000000000000000000000", &[seed; 32]).unwrap();
        NodeIdentity::from_seed(
            &aterm_net::fleet_member::node_principal(provisional.key()),
            &[seed; 32],
        )
        .unwrap()
    };
    let server = identity(11);
    let requester = identity(12);
    let secret = FleetSecret::new(&[7; 32]).unwrap();
    let listener = aterm_uds::kept_out(|| std::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, bound) = std::sync::mpsc::channel();
    let responder = Responder::start_on(
        address.port(),
        server.key(),
        || true,
        Network {
            scope: Scope::Loopback,
            port: discovery_port,
            sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
            ready: Some(ready),
        },
    )
    .unwrap();
    assert_eq!(bound.recv_timeout(HANG).unwrap(), 1);
    let until = Instant::now() + HANG;
    while !arrivals.poll().unwrap() {
        assert!(
            Instant::now() < until,
            "the late member announced no arrival"
        );
        aterm_uds::ready::any_until(&arrivals.fds(), until).unwrap();
    }
    assert!(!arrivals.poll().unwrap());
    // Admission becoming usable is a distinct hint on the same socket cohort. It
    // neither restarts discovery nor supplies an identity or replaces the handshake.
    let live = Arc::new(AtomicBool::new(true));
    responder.announce_ready(Arc::downgrade(&live));
    while !arrivals.poll().unwrap() {
        assert!(
            Instant::now() < until,
            "the admitted member announced no capability edge"
        );
        aterm_uds::ready::any_until(&arrivals.fds(), until).unwrap();
    }
    assert!(!arrivals.poll().unwrap());
    let hints = probe_on(
        links(&[Ipv4Addr::LOCALHOST], discovery_port, Scope::Loopback),
        HANG,
    )
    .unwrap();
    assert_eq!(
        hints,
        [Hint {
            address,
            key: server.key()
        }]
    );
    assert!(!responder.finished());
    let server_secret = secret.clone();
    let (done, finished) = std::sync::mpsc::channel();
    let server_task = thread::spawn(move || {
        // Prove the actual key twice: a copied or forged discovery claim
        // must not pass the ordinary membership-bound handshake.
        for _ in 0..2 {
            let (tcp, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            aterm_net::fleet::server_handshake_as(
                tcp,
                &server_secret,
                Some(&server),
                FleetScope::Facts,
                &[FleetOp::Survey],
                HANG,
            )
            .unwrap();
        }
        done.send(()).unwrap();
    });
    let keys = [(hints[0].key, aterm_net::fleet_member::Standing::Member)]
        .into_iter()
        .collect();
    for (claim, expected) in [(hints[0].key, true), (requester.key(), false)] {
        let advert = aterm_secrets::discover::Advert {
            instance: "direct-lan".into(),
            host: address.ip().to_string(),
            port: address.port(),
            txt: vec![
                ("v".into(), "2".into()),
                ("id".into(), aterm_net::fleet_member::node_principal(claim)),
                ("port".into(), address.port().to_string()),
            ],
        };
        assert_eq!(
            crate::node_service::prove_discovery_address(&advert, &secret, Some(&requester), &keys)
                .is_some(),
            expected
        );
    }
    finished.recv_timeout(HANG).unwrap();
    server_task.join().unwrap();
    drop(responder);
}

#[test]
fn sharing_hold_opens_no_discovery_socket_and_owner_drop_wakes_the_worker() {
    let mut inert = Arrivals::on(
        Network {
            scope: Scope::Loopback,
            port: 0,
            sources: Box::new(Vec::new),
            ready: None,
        },
        None,
    )
    .unwrap();
    assert!(inert.fds().is_empty());
    assert!(!inert.poll().unwrap());
    let (ready, bound) = std::sync::mpsc::channel();
    let responder = Responder::start_on(
        7100,
        NodeKey::from_bytes([3; 32]),
        || false,
        Network {
            scope: Scope::Loopback,
            port: 0,
            sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
            ready: Some(ready),
        },
    )
    .unwrap();
    assert_eq!(bound.recv_timeout(HANG).unwrap(), 0);
    assert!(!responder.finished());
    drop(responder);
}

/// When the responder next looks at its sources with nothing to prompt it. While it is allowed,
/// every source is served or refused by rule and the kernel's change socket is held, there is
/// no deadline: it waits for news. A deadline a second on is owed only where nothing would
/// prompt the look (no change socket, or something the look left owed), and a change of network
/// heard just before a look owes one more look when it has settled.
///
/// NEGATIVE CONTROLS (2026-11-12), each in `next_look`: with a second owed everywhere
/// (`.or_else(|| Some(now + REFRESH))`), `served and watched` fails; with no retry of what a
/// look left owed (`(!watched).then(..)`), `owed` fails; with the settled look dropped (no
/// `heard` arm), `a heard change settles` fails.
#[test]
fn the_next_look_has_a_deadline_only_where_nothing_else_would_prompt_it() {
    let heard = Instant::now();
    let now = heard + SETTLE;
    assert_eq!(
        next_look(now, true, false, None),
        None,
        "served and watched"
    );
    assert_eq!(
        next_look(now, true, true, None),
        Some(now + REFRESH),
        "owed"
    );
    for owed in [false, true] {
        assert_eq!(
            next_look(now, false, owed, None),
            Some(now + REFRESH),
            "no change socket"
        );
        assert_eq!(
            next_look(heard, true, owed, Some(heard)),
            Some(heard + SETTLE),
            "a heard change settles"
        );
        let last = now - Duration::from_nanos(1);
        assert_eq!(
            next_look(last, true, owed, Some(heard)),
            Some(heard + SETTLE)
        );
    }
    // A look begun once the change has settled is the settled look.
    assert_eq!(next_look(now, true, false, Some(heard)), None);
    assert_eq!(next_look(now, true, true, Some(heard)), Some(now + REFRESH));

    assert!(!bind_owed(&io::Error::new(
        io::ErrorKind::PermissionDenied,
        "a source this network's rule refuses"
    )));
    for kind in [
        io::ErrorKind::NotConnected,
        io::ErrorKind::AddrInUse,
        io::ErrorKind::AddrNotAvailable,
        io::ErrorKind::Other,
    ] {
        assert!(bind_owed(&io::Error::from(kind)), "{kind:?}");
    }
}

/// One kernel event message as `PF_SYSTEM` delivers it (Apple's, the network class's), for a
/// responder that hears a stand-in socket. On Linux it reads as a netlink message that names a
/// change, whatever its words.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn kernel_event(subclass: u32, code: u32) -> Vec<u8> {
    [24_u32, 1, 1, subclass, 37, code]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect()
}

/// A change socket that hears what a test writes, as the kernel's, to the socket beside it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stand_in() -> (Changes, std::os::unix::net::UnixDatagram) {
    let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    let changes = Changes(Some(aterm_uds::network::RouteChanges::over(heard.into())));
    (changes, kernel)
}

/// A datagram the responder ignores. A member that hears its own loop back knows the interface
/// delivered everything sent before it to every socket joined to the group
/// ([`queued_test_probe`]).
#[cfg(any(target_os = "macos", target_os = "linux"))]
const FENCE: &[u8] = b"aterm-m1-test-fence";

/// Read `member` up to the first datagram `wanted` takes, keeping each one read in `log`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn hear(member: &Multicast, log: &mut Vec<Vec<u8>>, mut wanted: impl FnMut(&[u8]) -> bool) {
    let until = Instant::now() + HANG;
    loop {
        assert!(
            !aterm_uds::ready::any_until(&[member.as_fd()], until)
                .unwrap()
                .is_empty(),
            "the member never heard what it waits for"
        );
        while let Some((wire, _)) = member.receive().unwrap() {
            let found = wanted(&wire);
            log.push(wire);
            if found {
                return;
            }
        }
    }
}

/// A responder's worker on a private loopback group port, turned on the test's own thread one
/// step at a time (a look at an instant the test names, or one wait and what it serves), so a
/// test reads what each step left owed and nothing depends on when a thread ran. Its change
/// socket is a stand-in: the worker waits with no deadline unless a look is owed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Turned {
    serving: Serving<Box<dyn Fn() -> bool>>,
    control: Arc<Control>,
    /// Another socket on the worker's group and port, and every datagram read from it.
    member: Multicast,
    log: Vec<Vec<u8>>,
    /// The addresses the worker's next look finds, and how many looks asked.
    sources: Arc<Mutex<Vec<Ipv4Addr>>>,
    looks: Arc<std::sync::atomic::AtomicUsize>,
    /// What `allowed` answers, and how many times it was asked.
    allowed: std::rc::Rc<std::cell::Cell<bool>>,
    asks: std::rc::Rc<std::cell::Cell<usize>>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Turned {
    /// A worker on a member's port, and the socket whose messages it hears as the kernel's word
    /// of a change: silent unless the test writes one, and held for as long as the worker.
    fn new(sources: Vec<Ipv4Addr>) -> (Self, std::os::unix::net::UnixDatagram) {
        let member = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
        let (changes, kernel) = stand_in();
        (Self::over(member.port(), member, sources, changes), kernel)
    }

    /// A worker on `port` that hears `changes`, beside `member`.
    fn over(port: u16, member: Multicast, sources: Vec<Ipv4Addr>, changes: Changes) -> Self {
        let sources = Arc::new(Mutex::new(sources));
        let looks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let allowed = std::rc::Rc::new(std::cell::Cell::new(true));
        let asks = std::rc::Rc::new(std::cell::Cell::new(0));
        let (stop, control) = Control::pipe().unwrap();
        let serving = Serving::new(
            stop,
            Arc::clone(&control),
            (7100, NodeKey::from_bytes([5; 32]), [6; NONCE]),
            Box::new({
                let (allowed, asks) = (allowed.clone(), asks.clone());
                move || {
                    asks.set(asks.get() + 1);
                    allowed.get()
                }
            }) as Box<dyn Fn() -> bool>,
            Network {
                scope: Scope::Loopback,
                port,
                sources: Box::new({
                    let (sources, looks) = (Arc::clone(&sources), Arc::clone(&looks));
                    move || {
                        looks.fetch_add(1, Ordering::Relaxed);
                        sources.lock().unwrap().clone()
                    }
                }),
                ready: None,
            },
            changes,
        );
        Self {
            serving,
            control,
            member,
            log: Vec::new(),
            sources,
            looks,
            allowed,
            asks,
        }
    }

    fn looks(&self) -> usize {
        self.looks.load(Ordering::Relaxed)
    }

    /// Look if a look is owed at `now`, as the worker's loop does before each wait.
    fn look(&mut self, now: Instant) {
        assert!(self.serving.look_if_due(now).is_continue());
    }

    /// One wait and what it serves, as the worker's loop makes after each look. The wait must
    /// have something to serve already: a turn that would wait for nothing is a test's mistake,
    /// or the loss of what the test turns for, caught by the one-minute detector here instead of
    /// a hung wait there. A look owed is no exception: a wait whose look is owed returns with
    /// nothing served once that instant has passed, and a test that turns until something is
    /// served ([`Self::turn_until_asked`]) would turn for ever.
    fn turn(&mut self) {
        let mut fds: Vec<_> = self
            .serving
            .sockets
            .iter()
            .map(|(_, link)| link.as_fd())
            .collect();
        fds.push(self.serving.stop.as_fd());
        fds.extend(self.serving.changes.fd());
        assert!(
            !aterm_uds::ready::any_until(&fds, Instant::now() + HANG)
                .unwrap()
                .is_empty(),
            "the worker has nothing to serve"
        );
        assert!(self.serving.wait_and_serve().is_continue());
    }

    /// Turn until `allowed` has been asked `asks` times in all. A worker that keeps finding
    /// something ready and never asks (a link whose receive fails stays readable until a look
    /// drops it) fails the one-minute detector here.
    fn turn_until_asked(&mut self, asks: usize) {
        let until = Instant::now() + HANG;
        while self.asks.get() < asks {
            assert!(
                Instant::now() < until,
                "the worker served for a minute and never asked whether it is allowed"
            );
            self.turn();
        }
    }

    /// Send `wire` to the group from the member and return once it waits on every socket
    /// joined there, the worker's links among them ([`FENCE`]).
    fn deliver(&mut self, wire: &[u8]) {
        self.member.send(wire).unwrap();
        self.member.send(FENCE).unwrap();
        hear(&self.member, &mut self.log, |heard| heard == FENCE);
    }

    /// Read the member up to the worker's answer to the request `nonce` names, and say which
    /// requests the member has heard answered, in the order their answers came.
    fn answered_up_to(&mut self, nonce: [u8; NONCE]) -> Vec<u8> {
        let source: SocketAddr = (Ipv4Addr::LOCALHOST, PORT).into();
        hear(&self.member, &mut self.log, |heard| {
            hint(heard, source, &nonce).is_some()
        });
        self.log
            .iter()
            .filter(|wire| wire.len() == RESPONSE)
            .map(|wire| wire[MAGIC.len()])
            .collect()
    }
}

/// A change of network the kernel names is what prompts a look once the responder waits with no
/// deadline: the look comes at once and binds and announces the address the change brought,
/// and one more comes when the change has settled. A message that names no change (a multicast
/// join) prompts none.
///
/// NEGATIVE CONTROL (2026-11-12): with a heard change owing no look (`Serving::hear`'s
/// `Some(true)` arm leaving `look` as it was), `a heard change owes a look now` fails.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_heard_address_change_binds_and_announces_and_is_looked_at_once_more() {
    let (mut turned, kernel) = Turned::new(Vec::new());
    turned.look(Instant::now());
    assert_eq!((turned.looks(), turned.serving.sockets.len()), (1, 0));
    assert_eq!(
        turned.serving.look, None,
        "no source and a change socket: no deadline"
    );

    #[cfg(target_os = "macos")]
    {
        kernel.send(&kernel_event(2, 7)).expect("a multicast join");
        turned.turn();
        assert_eq!(
            (turned.serving.look, turned.serving.heard),
            (None, None),
            "a multicast join is no change of network"
        );
    }

    *turned.sources.lock().unwrap() = vec![Ipv4Addr::LOCALHOST];
    kernel.send(&kernel_event(1, 1)).expect("an address change");
    turned.turn();
    let heard = turned.serving.heard.expect("the change was heard");
    assert_eq!(
        turned.serving.look,
        Some(heard),
        "a heard change owes a look now"
    );
    assert_eq!(turned.looks(), 1, "which the wait itself does not make");

    turned.look(heard);
    assert_eq!((turned.looks(), turned.serving.sockets.len()), (2, 1));
    assert_eq!(
        turned.serving.look,
        Some(heard + SETTLE),
        "and one more when it has settled"
    );
    hear(&turned.member, &mut turned.log, |wire| {
        ready_token(wire).is_some()
    });

    turned.look(heard + SETTLE - Duration::from_nanos(1));
    assert_eq!(turned.looks(), 2, "no look before it is owed");
    turned.look(heard + SETTLE);
    assert_eq!((turned.looks(), turned.serving.sockets.len()), (3, 1));
    assert_eq!(
        (turned.serving.look, turned.serving.generation),
        (None, 1),
        "the settled look finds its link held, announces nothing and owes no other"
    );
    assert_eq!(turned.serving.heard, None, "and the change is settled");
}

/// The settled look a heard change owes stays owed until it is made. A look something else
/// prompts inside the settle window (here a link whose receive failed) is begun too soon to be
/// it, so it leaves the settled look owed at the same instant: the worker never waits with no
/// deadline on what a change had not yet brought.
///
/// NEGATIVE CONTROL (2026-11-12): with the change forgotten at the first look begun after it
/// (`Serving::look_if_due` handing `self.heard.take()` to `next_look`), `the settled look is
/// still owed` fails: the look the failed link prompted leaves no deadline.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_look_prompted_inside_the_settle_window_leaves_the_settled_look_owed() {
    let (mut turned, kernel) = Turned::new(vec![Ipv4Addr::LOCALHOST]);
    turned.look(Instant::now());
    kernel.send(&kernel_event(1, 1)).expect("an address change");
    turned.turn();
    let heard = turned.serving.heard.expect("the change was heard");
    turned.look(heard);
    assert_eq!(
        (turned.looks(), turned.serving.look),
        (2, Some(heard + SETTLE))
    );

    turned.deliver(&request(&[1; NONCE]));
    turned.serving.sockets[0].1.withdraw();
    turned.turn();
    assert!(
        turned.serving.look.is_some_and(|at| at <= Instant::now()),
        "a link whose receive failed owes a look now"
    );
    // The wait dated that look by its own clock. The test names an instant inside the settle
    // window in its place, so nothing rests on how soon after the change the wait ran.
    let early = heard + SETTLE / 2;
    turned.serving.look = Some(early);
    turned.look(early);
    assert_eq!((turned.looks(), turned.serving.sockets.len()), (3, 1));
    assert_eq!(
        (turned.serving.look, turned.serving.heard),
        (Some(heard + SETTLE), Some(heard)),
        "the settled look is still owed"
    );

    turned.look(heard + SETTLE);
    assert_eq!(turned.looks(), 4);
    assert_eq!(
        (turned.serving.look, turned.serving.heard),
        (None, None),
        "and once made, nothing more is"
    );
}

/// A change of network is looked at before anything is answered: a request that waits on a link
/// when the change is heard stays queued through that wait, and is answered after the look by
/// the links the look kept.
///
/// NEGATIVE CONTROLS (2026-11-12), each in `Serving::wait_and_serve`: with the answers made in
/// the wait that heard the change (no `if self.put_off` return), `nothing is answered before
/// the look` fails; with that wait reading what its links hold and dropping it, the request is
/// never answered and the one-minute detector in `Turned::turn` fails the test (`the worker has
/// nothing to serve`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_heard_change_is_looked_at_before_a_waiting_request_is_answered() {
    let (mut turned, kernel) = Turned::new(vec![Ipv4Addr::LOCALHOST]);
    turned.look(Instant::now());
    assert_eq!(
        (turned.serving.sockets.len(), turned.serving.look),
        (1, None)
    );

    let nonce = [1; NONCE];
    turned.deliver(&request(&nonce));
    kernel.send(&kernel_event(1, 1)).expect("an address change");
    let asks = turned.asks.get();
    turned.turn();
    assert_eq!(
        turned.asks.get(),
        asks,
        "nothing is answered before the look"
    );
    let heard = turned.serving.heard.expect("the change was heard");

    turned.look(heard);
    assert_eq!(
        (turned.looks(), turned.asks.get()),
        (2, asks + 1),
        "the look asks once and binds nothing new"
    );
    turned.turn_until_asked(asks + 2);
    assert_eq!(turned.answered_up_to(nonce), [1]);
}

/// Changes of network that keep coming (an interface that flaps) put the answers off by one
/// look and no more. The wait that hears the first change serves no link; the wait after it
/// answers what is queued although it hears a second change, which still owes its look now; and
/// the wait after a serving one looks first again.
///
/// NEGATIVE CONTROL (2026-11-12): with every wait that hears a change given to it
/// (`self.put_off = changed` in `Serving::wait_and_serve`), `the wait after a put-off one
/// answers` fails: a change at every wait would leave the request unread for as long as they
/// come.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn changes_that_keep_coming_put_the_answers_off_by_one_look() {
    let (mut turned, kernel) = Turned::new(vec![Ipv4Addr::LOCALHOST]);
    turned.look(Instant::now());

    let nonce = [1; NONCE];
    turned.deliver(&request(&nonce));
    kernel.send(&kernel_event(1, 1)).expect("an address change");
    let asks = turned.asks.get();
    turned.turn();
    assert_eq!(
        turned.asks.get(),
        asks,
        "the first change is looked at first"
    );
    let first = turned.serving.heard.expect("the change was heard");
    turned.look(first);
    assert_eq!(
        (turned.asks.get(), turned.serving.look),
        (asks + 1, Some(first + SETTLE)),
        "the look asks once and binds nothing new"
    );

    kernel.send(&kernel_event(1, 1)).expect("a second change");
    turned.turn();
    assert_eq!(
        turned.asks.get(),
        asks + 2,
        "the wait after a put-off one answers"
    );
    assert_eq!(turned.answered_up_to(nonce), [1]);
    let second = turned.serving.heard.expect("the second change was heard");
    assert_eq!(
        turned.serving.look,
        Some(second),
        "and the change it heard owes a look now"
    );

    turned.look(second);
    turned.deliver(&request(&[2; NONCE]));
    kernel.send(&kernel_event(1, 1)).expect("a third change");
    let asks = turned.asks.get();
    turned.turn();
    assert_eq!(
        turned.asks.get(),
        asks,
        "a change heard by the wait after a serving one is looked at first again"
    );
}

/// `allowed` is asked before every answer, with nothing kept from the ask before: of three
/// requests made while it says yes, no and yes, the first and third are answered. The refused
/// one owes a look now, since no timer comes: that look holds no link while it is refused and
/// comes again a second on, and the look that finds the responder allowed again binds and
/// announces as a first look does.
///
/// NEGATIVE CONTROLS (2026-11-12), each in `Serving::wait_and_serve`: with a refused request
/// answered all the same (the `continue` after `allowed` says no removed), the member hears
/// the second answer; with a refused answer owing no look (`look = true` removed there), `a
/// refused answer owes a look` fails.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn each_request_is_answered_only_while_allowed_and_a_refused_one_owes_a_look() {
    let (mut turned, _kernel) = Turned::new(vec![Ipv4Addr::LOCALHOST]);
    turned.look(Instant::now());

    let asks = turned.asks.get();
    turned.deliver(&request(&[1; NONCE]));
    turned.turn_until_asked(asks + 1);
    assert_eq!(
        turned.serving.look, None,
        "an answered request owes no look"
    );

    turned.allowed.set(false);
    turned.deliver(&request(&[2; NONCE]));
    turned.turn_until_asked(asks + 2);
    let refused = turned.serving.look.expect("a refused answer owes a look");
    assert!(refused <= Instant::now(), "now");
    turned.look(refused);
    assert_eq!(
        (turned.serving.sockets.len(), turned.serving.look),
        (0, Some(refused + REFRESH)),
        "a refused look holds no link and comes again"
    );

    turned.allowed.set(true);
    turned.look(refused + REFRESH);
    assert_eq!(
        (turned.serving.sockets.len(), turned.serving.look),
        (1, None),
        "allowed again: bound, and no deadline"
    );
    assert_eq!(turned.serving.generation, 2, "and announced anew");

    let asks = turned.asks.get();
    turned.deliver(&request(&[3; NONCE]));
    turned.turn_until_asked(asks + 1);
    assert_eq!(turned.answered_up_to([3; NONCE]), [1, 3]);
}

/// A link that fails owes a look now. A withdrawn link refuses its receive before reading, so
/// the datagram that made it readable stays queued: with no timer to bring a look, a worker
/// that only passed over the error would find the same link readable at every wait, a spin.
/// The look drops the link and binds its source again, and the worker then waits with no
/// deadline on a link that serves.
///
/// NEGATIVE CONTROL (2026-11-12): with a failed receive passed over (`look = true` removed from
/// the `Err` arm in `Serving::wait_and_serve`), `a link whose receive failed owes a look` fails:
/// no look is owed, and the worker's loop would find the link readable at every wait.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_withdrawn_link_with_a_datagram_queued_owes_a_look_and_the_worker_then_waits() {
    let (mut turned, _kernel) = Turned::new(vec![Ipv4Addr::LOCALHOST]);
    turned.look(Instant::now());
    assert_eq!((turned.looks(), turned.serving.look), (1, None));

    turned.deliver(&request(&[1; NONCE]));
    turned.serving.sockets[0].1.withdraw();
    let asks = turned.asks.get();
    turned.turn();
    let failed = turned
        .serving
        .look
        .expect("a link whose receive failed owes a look");
    assert!(failed <= Instant::now(), "now");
    assert_eq!(
        (turned.looks(), turned.asks.get()),
        (1, asks),
        "its request went unread"
    );

    turned.look(failed);
    assert_eq!((turned.looks(), turned.serving.sockets.len()), (2, 1));
    assert!(
        turned.serving.sockets[0].1.recheck().is_ok(),
        "the look dropped the withdrawn link and bound its source again"
    );
    assert_eq!(turned.serving.look, None, "and the worker waits");

    let asks = turned.asks.get();
    turned.deliver(&request(&[2; NONCE]));
    turned.turn_until_asked(asks + 1);
    assert_eq!(turned.answered_up_to([2; NONCE]), [2]);
    assert_eq!((turned.looks(), turned.serving.look), (2, None));
}

/// A send that fails owes a look as a receive does: here the announcement of a listener
/// generation, on a link withdrawn with nothing queued on it (the test reads the link's own
/// first announcement off it first), so only the send can tell.
///
/// NEGATIVE CONTROL (2026-11-12): with a failed announcement passed over (`failed |=` dropped
/// in `Serving::announce`), `an announcement that could not be sent owes a look` fails.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_announcement_that_cannot_be_sent_owes_a_look() {
    let (mut turned, _kernel) = Turned::new(vec![Ipv4Addr::LOCALHOST]);
    turned.look(Instant::now());
    let link = &turned.serving.sockets[0].1;
    assert!(
        !aterm_uds::ready::any_until(&[link.as_fd()], Instant::now() + HANG)
            .unwrap()
            .is_empty(),
        "a link hears its own announcement"
    );
    let (own, _) = link.receive().unwrap().expect("its own announcement");
    assert!(ready_token(&own).is_some());
    link.withdraw();
    assert_eq!(turned.serving.look, None);

    let live = Arc::new(AtomicBool::new(true));
    turned.control.announce(Arc::downgrade(&live));
    turned.turn();
    let failed = turned
        .serving
        .look
        .expect("an announcement that could not be sent owes a look");
    turned.look(failed);
    assert_eq!((turned.looks(), turned.serving.sockets.len()), (2, 1));
    assert!(turned.serving.sockets[0].1.recheck().is_ok());
}

/// What a look leaves owed when a source has no link. A source this network's rule refuses (a
/// LAN address on a loopback network here; a loopback or tunnel address on a LAN) owes nothing:
/// the responder still waits with no deadline. A bind the OS refused (here the port, held by a
/// socket that does not share it) is tried again a second on, with no change of network to
/// prompt it, and binds once the port is free.
///
/// NEGATIVE CONTROLS (2026-11-12): with every failed bind owed (`bind_owed` answering `true`),
/// `a source refused by rule owes no look` fails; with none owed (`false`), `a failed bind is
/// tried again` fails.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_source_refused_by_rule_owes_no_look_and_a_bind_that_failed_is_tried_again() {
    let (mut turned, _kernel) = Turned::new(vec![Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::LOCALHOST]);
    turned.look(Instant::now());
    assert_eq!(
        (turned.serving.sockets.len(), turned.serving.look),
        (1, None),
        "a source refused by rule owes no look"
    );

    let holder =
        aterm_uds::kept_out(|| std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))).unwrap();
    let (changes, _kernel) = stand_in();
    let member = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    let mut turned = Turned::over(
        holder.local_addr().unwrap().port(),
        member,
        vec![Ipv4Addr::LOCALHOST],
        changes,
    );
    let start = Instant::now();
    turned.look(start);
    assert_eq!(
        (turned.serving.sockets.len(), turned.serving.look),
        (0, Some(start + REFRESH)),
        "a failed bind is tried again"
    );
    drop(holder);
    turned.look(start + REFRESH);
    assert_eq!(
        (
            turned.looks(),
            turned.serving.sockets.len(),
            turned.serving.look
        ),
        (2, 1, None)
    );
}

/// A change socket that fails is dropped (it would poll readable again at once) and owes a look
/// now, from which the responder looks every second as one that never held a socket does.
///
/// NEGATIVE CONTROL (2026-11-12): with the failed socket owing no look (`Serving::hear`'s `None`
/// arm leaving `look` as it was), `a failed change socket owes a look` fails: the worker would
/// wait with no deadline and hear of no change.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_change_socket_that_fails_leaves_the_responder_looking_every_second() {
    let (heard, peer) = std::os::unix::net::UnixStream::pair().expect("a stream pair");
    drop(peer);
    let failed = Changes(Some(aterm_uds::network::RouteChanges::over(heard.into())));
    let member = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    let mut turned = Turned::over(member.port(), member, vec![Ipv4Addr::LOCALHOST], failed);
    turned.look(Instant::now());
    assert_eq!(turned.serving.look, None, "held: no deadline yet");

    turned.turn();
    assert!(!turned.serving.changes.held(), "a failed socket is dropped");
    let deaf = turned
        .serving
        .look
        .expect("a failed change socket owes a look");
    turned.look(deaf);
    assert_eq!(
        (turned.looks(), turned.serving.look),
        (2, Some(deaf + REFRESH))
    );
}

/// The worker's own loop, on its thread, with a change socket that stays silent: a look that
/// `allowed` refuses while a source exists is made again with no event to prompt it, so a
/// responder that becomes allowed binds and announces. Nothing but the retry can bring that
/// look: no change is ever written to the stand-in.
///
/// NEGATIVE CONTROL (2026-11-12): with no retry of what a look left owed (`(!watched).then(..)`
/// in `next_look`), the worker waits with no deadline after its refused look and the member
/// never hears the announcement: the one-minute detector in `hear` fails the test.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_refused_look_is_made_again_with_no_event_to_prompt_it() {
    let member = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    let (changes, kernel) = stand_in();
    let allowed = Arc::new(AtomicBool::new(false));
    let (ready, bound) = std::sync::mpsc::channel();
    let responder = Responder::start_over(
        7100,
        NodeKey::from_bytes([3; 32]),
        {
            let allowed = Arc::clone(&allowed);
            move || allowed.load(Ordering::Acquire)
        },
        Network {
            scope: Scope::Loopback,
            port: member.port(),
            sources: Box::new(|| vec![Ipv4Addr::LOCALHOST]),
            ready: Some(ready),
        },
        changes,
    )
    .unwrap();
    assert_eq!(
        bound.recv_timeout(HANG).unwrap(),
        0,
        "the first look is refused"
    );
    allowed.store(true, Ordering::Release);
    hear(&member, &mut Vec::new(), |wire| ready_token(wire).is_some());
    assert!(!responder.finished());
    drop(responder);
    drop(kernel);
}

/// The worker's own loop hears the kernel's word: with no source at its first look it waits
/// with no deadline, and an address change written to its change socket brings the look that
/// binds the address the change brought and announces it.
///
/// NEGATIVE CONTROL (2026-11-12): with a heard change owing no look (`Serving::hear`'s
/// `Some(true)` arm leaving `look` as it was), the member never hears the announcement: the
/// one-minute detector in `hear` fails the test.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_worker_waits_for_a_change_of_network_and_then_binds_and_announces() {
    let member = Multicast::bind(Ipv4Addr::LOCALHOST, GROUP, 0, Scope::Loopback).unwrap();
    let (changes, kernel) = stand_in();
    let sources = Arc::new(Mutex::new(Vec::new()));
    let (ready, bound) = std::sync::mpsc::channel();
    let responder = Responder::start_over(
        7100,
        NodeKey::from_bytes([3; 32]),
        || true,
        Network {
            scope: Scope::Loopback,
            port: member.port(),
            sources: Box::new({
                let sources = Arc::clone(&sources);
                move || sources.lock().unwrap().clone()
            }),
            ready: Some(ready),
        },
        changes,
    )
    .unwrap();
    assert_eq!(
        bound.recv_timeout(HANG).unwrap(),
        0,
        "no source at its first look"
    );
    *sources.lock().unwrap() = vec![Ipv4Addr::LOCALHOST];
    kernel.send(&kernel_event(1, 1)).expect("an address change");
    hear(&member, &mut Vec::new(), |wire| ready_token(wire).is_some());
    assert!(!responder.finished());
    drop(responder);
}
