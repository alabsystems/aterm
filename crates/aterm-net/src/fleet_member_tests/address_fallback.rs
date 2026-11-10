// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A target's address walk keeps looking after every rejected identity.

use super::*;
use std::sync::{Mutex, mpsc};

const SERVES: &[FleetOp] = &[FleetOp::Survey, FleetOp::Drive];

struct Peer {
    addr: SocketAddr,
    asked: Arc<AtomicUsize>,
    stop: Arc<Stop>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn start(node: NodeIdentity, serves: &'static [FleetOp]) -> Self {
        Self::start_notifying(node, serves, None)
    }

    fn start_notifying(
        node: NodeIdentity,
        serves: &'static [FleetOp],
        proved: Option<mpsc::Sender<()>>,
    ) -> Self {
        Self::start_after_drops(node, serves, proved, 0)
    }

    fn start_after_drops(
        node: NodeIdentity,
        serves: &'static [FleetOp],
        proved: Option<mpsc::Sender<()>>,
        mut drops: usize,
    ) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(Stop::new());
        let asked = Arc::new(AtomicUsize::new(0));
        let thread = std::thread::spawn({
            let stop = Arc::clone(&stop);
            let asked = Arc::clone(&asked);
            move || {
                while drops > 0 && !stop.is_set() {
                    match aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Ready,
                        aterm_uds::AcceptedMode::Nonblocking,
                    ) {
                        Ok(connection) => {
                            drop(connection);
                            drops -= 1;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("accepting a dropped handshake: {error}"),
                    }
                }
                if stop.is_set() {
                    return;
                }
                let node_name = node.node().to_owned();
                let listening = Listening {
                    secret: secret(6),
                    scope: FleetScope::Content,
                    node: Some(node),
                    serves,
                    slots: 8,
                    per_source: 8,
                    lifetime: HANG,
                };
                serve_with_handshake(
                    &listener,
                    &listening,
                    &stop,
                    HANG,
                    move |served, op| {
                        if let Some(proved) = &proved {
                            let _ = proved.send(());
                        }
                        served.answer(|stream| {
                            stream.set_read_timeout(Some(HANG))?;
                            let mut byte = [0u8; 1];
                            let mut request = Vec::new();
                            while stream.read(&mut byte)? == 1 {
                                request.push(byte[0]);
                                if byte[0] == b'\n' {
                                    break;
                                }
                            }
                            if request.is_empty() {
                                return Ok(());
                            }
                            asked.fetch_add(1, Ordering::AcqRel);
                            if op == FleetOp::Survey {
                                stream.write_all(
                                    format!(
                                        "OK 1\ninstance pid=1 host=target version=0.148.0 \
                                         sessions=0 level=content cut=0 at_ms=1 node={node_name}\n"
                                    )
                                    .as_bytes(),
                                )?;
                            } else {
                                stream.write_all(b"OK reached\n")?;
                            }
                            stream.flush()
                        })
                    },
                    |_: FleetEvent| {},
                );
            }
        });
        Self {
            addr,
            asked,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

static RESOLVED: AtomicUsize = AtomicUsize::new(0);

fn addresses(host: &str, _: u16) -> Vec<SocketAddr> {
    RESOLVED.fetch_add(1, Ordering::AcqRel);
    host.split(',').map(|word| word.parse().unwrap()).collect()
}

fn drive_pair(
    first: &Peer,
    second: &Peer,
    want: &NodeIdentity,
    me: &NodeIdentity,
) -> (Result<Driven, Unreached>, usize) {
    let target = Target {
        host: second.addr.to_string(),
        addr: Some(first.addr.ip()),
        port: first.addr.port(),
        node: Some(want.node().to_owned()),
        key: Some(want.key()),
    };
    let deadline = (Instant::now() + HANG, HANG);
    let mut tried = 0;
    let result = passes((deadline.0, deadline.0), |by| {
        tried += 1;
        reach_node_pass(
            &target,
            &Keyring::default(),
            (&secret(6), me),
            FleetOp::Drive,
            (by, deadline.1),
            |host, _| vec![host.parse().unwrap()],
        )
    })
    .and_then(|(reached, node)| driven(reached, node, deadline));
    (result, tried)
}

/// The cached address proves another node; the real endpoint hangs up once, then serves.
/// A retained identity diagnostic must not suppress that endpoint's safe handshake retry.
/// NEGATIVE CONTROL: deriving pass retryability only from its final `why` ends after one pass.
#[test]
fn a_wrong_identity_does_not_hide_a_transient_target_from_drive_retries() {
    let want = identity(&node_of(0x81), 1);
    let me = identity(&node_of(0x82), 2);
    let wrong = Peer::start(identity(&node_of(0x83), 3), SERVES);
    let target = Peer::start_after_drops(want.clone(), SERVES, None, 1);
    let (result, tried) = drive_pair(&wrong, &target, &want, &me);
    let mut driven = result.unwrap();
    assert_eq!(tried, 2);
    assert_eq!(driven.addr, target.addr);
    assert_eq!(driven.node, want.node());
    assert_eq!(driven.key, want.key());
    driven.write_all(b"@s-1 status\n").unwrap();
    let mut reply = String::new();
    io::BufRead::read_line(&mut io::BufReader::new(&mut driven), &mut reply).unwrap();
    assert_eq!(reply, "OK reached\n");
    assert_eq!(wrong.asked.load(Ordering::Acquire), 0);
    assert_eq!(target.asked.load(Ordering::Acquire), 1);
}

#[test]
fn exhausted_drive_retries_keep_the_first_identity_diagnostic() {
    let want = identity(&node_of(0x84), 4);
    let me = identity(&node_of(0x85), 5);
    let wrong_node = identity(&node_of(0x86), 6);
    let wrong = Peer::start(wrong_node.clone(), SERVES);
    let target = Peer::start_after_drops(want.clone(), SERVES, None, AGAIN_AFTER.len() + 1);
    let (result, tried) = drive_pair(&wrong, &target, &want, &me);
    assert_eq!(tried, AGAIN_AFTER.len() + 1);
    assert!(
        matches!(result, Err(Unreached::OtherNode { at, said, want: wanted })
        if at == wrong.addr && said == wrong_node.node() && wanted == want.node())
    );
    assert_eq!(wrong.asked.load(Ordering::Acquire), 0);
    assert_eq!(target.asked.load(Ordering::Acquire), 0);
}

#[test]
fn identities_alone_do_not_make_a_drive_retry() {
    let want = identity(&node_of(0x87), 7);
    let me = identity(&node_of(0x88), 8);
    let first = Peer::start(identity(&node_of(0x89), 9), SERVES);
    let second = Peer::start(identity(&node_of(0x8a), 10), SERVES);
    let (result, tried) = drive_pair(&first, &second, &want, &me);
    assert_eq!(tried, 1);
    assert!(matches!(result, Err(Unreached::OtherNode { at, .. }) if at == first.addr));
    assert_eq!(first.asked.load(Ordering::Acquire), 0);
    assert_eq!(second.asked.load(Ordering::Acquire), 0);
}

#[test]
fn a_bound_refusal_overrides_a_passed_handshakes_retry_evidence() {
    let want = identity(&node_of(0x8b), 11);
    let me = identity(&node_of(0x8c), 12);
    let first = Peer::start_after_drops(identity(&node_of(0x8d), 13), SERVES, None, 1);
    let refusing = Peer::start(want.clone(), &[FleetOp::Survey]);
    let (result, tried) = drive_pair(&first, &refusing, &want, &me);
    assert_eq!(tried, 1);
    assert!(
        matches!(result, Err(Unreached::Handshake(at, FleetError::UnknownOp(op)))
        if at == refusing.addr && op == "drive")
    );
    assert_eq!(first.asked.load(Ordering::Acquire), 0);
    assert_eq!(refusing.asked.load(Ordering::Acquire), 0);
}

#[test]
fn bound_target_refusals_keep_their_own_retry_disposition() {
    let want = identity(&node_of(0x8e), 14);
    let first = SocketAddr::from((Ipv4Addr::LOCALHOST, 7101));
    let second = SocketAddr::from((Ipv4Addr::LOCALHOST, 7102));
    let third = SocketAddr::from((Ipv4Addr::LOCALHOST, 7103));
    let target = Target {
        host: "target.invalid".into(),
        addr: Some(first.ip()),
        port: first.port(),
        node: Some(want.node().into()),
        key: Some(want.key()),
    };
    for (error, retryable) in [
        (FleetError::AddressBusy, true),
        (FleetError::DenyListUnread, true),
        (FleetError::DeniedNode, false),
        (FleetError::UnknownOp("drive".into()), false),
    ] {
        let mut refusal = Some(error);
        let result: Result<(), _> = walk_target_pass(
            &target,
            &Keyring::default(),
            (Instant::now() + HANG, HANG),
            move |_, _| vec![second, third],
            |at| {
                if at == first {
                    return AtAddr::Passed(Unreached::Deadline(Stage::Handshake, HANG));
                }
                assert_eq!(at, second, "the bound target ends the address walk");
                AtAddr::Refused(ProvedRefusal {
                    why: Unreached::Handshake(at, refusal.take().unwrap()),
                    peer: NodeProof {
                        node: want.node().into(),
                        key: want.key(),
                    },
                    ended: false,
                })
            },
        );
        let failed = result.unwrap_err();
        assert!(matches!(failed.why, Unreached::Handshake(at, _) if at == second));
        assert_eq!(failed.retryable, retryable);
    }
}

/// Retry evidence is independent of whichever nonterminal dial error remains the diagnostic.
/// Failures saying this machine could not dial are still not made retryable by another address.
#[test]
fn passed_transients_support_other_dial_errors_but_not_local_refusals() {
    let first = SocketAddr::from((Ipv4Addr::LOCALHOST, 7101));
    let second = SocketAddr::from((Ipv4Addr::LOCALHOST, 7102));
    for local in [false, true] {
        let deadline = Instant::now() + HANG;
        let mut tried = 0;
        let result = passes((deadline, deadline), |by| {
            tried += 1;
            walk_pass(
                ("two-addresses.invalid", Some(first.ip()), first.port()),
                (by, HANG),
                (&[], move |_, _| vec![second]),
                |at| {
                    if tried > 1 {
                        return AtAddr::Reached(());
                    }
                    AtAddr::Passed(if at == first {
                        if local {
                            Unreached::Local("local refusal".into())
                        } else {
                            Unreached::Handshake(at, FleetError::Alpn)
                        }
                    } else {
                        Unreached::Deadline(Stage::Handshake, HANG)
                    })
                },
            )
        });
        assert_eq!(tried, if local { 1 } else { 2 });
        if local {
            assert!(matches!(result, Err(Unreached::Local(_))));
        } else {
            assert!(result.is_ok(), "{result:?}");
        }
    }
}

#[test]
fn a_failed_lookup_keeps_only_the_retained_reasons_existing_retryability() {
    let first = SocketAddr::from((Ipv4Addr::LOCALHOST, 7101));
    let second = SocketAddr::from((Ipv4Addr::LOCALHOST, 7102));
    for (kind, identity) in (0..3).flat_map(|kind| [(kind, true), (kind, false)]) {
        let failed_lookup = match kind {
            0 => Lookup::Late,
            1 => Lookup::Unstarted(io::ErrorKind::WouldBlock.into()),
            _ => Lookup::Refused(io::ErrorKind::PermissionDenied.into()),
        };
        let result: Result<(), _> = walk_lookups(
            ("two-addresses.invalid", Some(first.ip()), first.port()),
            HANG,
            &[],
            || [Lookup::Found(vec![second]), failed_lookup],
            |at| {
                AtAddr::Passed(if at == first && identity {
                    Unreached::OtherNode {
                        at,
                        said: node_of(0x91),
                        want: node_of(0x92),
                    }
                } else {
                    Unreached::Deadline(Stage::Handshake, HANG)
                })
            },
        );
        let failed = result.unwrap_err();
        assert_eq!(failed.retryable, !identity);
        if identity {
            assert!(matches!(failed.why, Unreached::OtherNode { at, .. } if at == first));
        } else {
            assert!(matches!(
                failed.why,
                Unreached::Deadline(Stage::Handshake, _)
            ));
        }
    }
}

/// Three real listeners prove A, B and C under the same fleet secret. A/B never receive an
/// application request. C is accepted only when it proves the target's bound key; otherwise the
/// first identity error stands. The same walk serves Drive and the coalesced survey path.
#[test]
fn every_wrong_address_gives_way_to_the_bound_target() {
    let me = identity(&node_of(0xf0), 9);
    let want = identity(&node_of(0xa0), 1);
    let wrong_a = identity(&node_of(0xb0), 2);
    let wrong_b = identity(&node_of(0xc0), 3);
    let wrong_c = identity(&node_of(0xd0), 4);
    for correct in [true, false] {
        let a = Peer::start(wrong_a.clone(), SERVES);
        let b = Peer::start(wrong_b.clone(), SERVES);
        let c = Peer::start(
            if correct {
                want.clone()
            } else {
                wrong_c.clone()
            },
            SERVES,
        );
        let target = Target {
            host: format!("{},{}", b.addr, c.addr),
            addr: Some(a.addr.ip()),
            port: a.addr.port(),
            node: Some(want.node().to_owned()),
            key: Some(want.key()),
        };
        for op in [FleetOp::Drive, FleetOp::Survey] {
            RESOLVED.store(0, Ordering::Release);
            let deadline = (Instant::now() + HANG, HANG);
            let reached = if op == FleetOp::Drive {
                reach_node_with(
                    &target,
                    &Keyring::default(),
                    (&secret(6), &me),
                    op,
                    deadline,
                    addresses,
                )
                .and_then(|(reached, node)| {
                    let mut stream = driven(reached, node, deadline)?;
                    stream.write_all(b"@s-1 status\n").unwrap();
                    let mut answer = String::new();
                    io::BufRead::read_line(&mut io::BufReader::new(&mut stream), &mut answer)
                        .unwrap();
                    assert_eq!(answer, "OK reached\n");
                    Ok(stream.addr)
                })
            } else {
                read_one_with(
                    &target,
                    &Keyring::default(),
                    (&secret(6), Some(&me)),
                    deadline,
                    "survey",
                    &HeldAddrs::default(),
                    addresses,
                )
                .map(|survey| {
                    assert!(survey.proven);
                    assert_eq!(survey.node.as_deref(), Some(want.node()));
                    survey.addr
                })
            };
            if correct {
                assert_eq!(reached.unwrap(), c.addr);
            } else {
                assert!(
                    matches!(reached, Err(Unreached::OtherNode { at, said, want: target })
                        if at == a.addr && said == wrong_a.node() && target == want.node()),
                    "the first wrong identity remains the diagnostic"
                );
            }
            assert_eq!(RESOLVED.load(Ordering::Acquire), 1, "one address walk");
        }
        assert_eq!(a.asked.load(Ordering::Acquire), 0);
        assert_eq!(b.asked.load(Ordering::Acquire), 0);
        assert_eq!(c.asked.load(Ordering::Acquire), if correct { 2 } else { 0 });
    }

    let a = Peer::start(wrong_a, SERVES);
    let b = Peer::start(wrong_b, SERVES);
    let c = Peer::start(want.clone(), &[FleetOp::Survey]);
    let target = Target {
        host: format!("{},{}", b.addr, c.addr),
        addr: Some(a.addr.ip()),
        port: a.addr.port(),
        node: Some(want.node().to_owned()),
        key: Some(want.key()),
    };
    RESOLVED.store(0, Ordering::Release);
    let refused = reach_node_with(
        &target,
        &Keyring::default(),
        (&secret(6), &me),
        FleetOp::Drive,
        (Instant::now() + HANG, HANG),
        addresses,
    );
    assert!(
        matches!(refused, Err(Unreached::Handshake(at, FleetError::UnknownOp(op)))
        if at == c.addr && op == "drive")
    );
    assert_eq!(RESOLVED.load(Ordering::Acquire), 1);
    for peer in [&a, &b, &c] {
        assert_eq!(peer.asked.load(Ordering::Acquire), 0);
    }
}

/// A proved refusal from the target is more authoritative than a stale address's identity.
/// In particular a busy target must remain retryable by Drive's existing passes.
#[test]
fn shared_refusals_are_judged_by_each_targets_key() {
    let real = identity(&node_of(0xa5), 5);
    let wrong = identity(&node_of(0xb5), 6);
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, 47111));
    let target = |node: &NodeIdentity| Target {
        host: "127.0.0.1".to_owned(),
        addr: Some(at.ip()),
        port: at.port(),
        node: Some(node.node().to_owned()),
        key: Some(node.key()),
    };
    for error in [
        FleetError::AddressBusy,
        FleetError::DeniedNode,
        FleetError::UnknownOp("drive".into()),
    ] {
        let held = HeldAddrs::default();
        let opened = held.enter(at).unwrap_or_else(|_| panic!("first exchange"));
        let deadline = (Instant::now() + HANG, HANG);
        let read = |target: &Target| {
            read_one_with(
                target,
                &Keyring::default(),
                (&secret(6), None),
                deadline,
                "survey",
                &held,
                |_, _| Vec::new(),
            )
        };
        let (a, b, c) = std::thread::scope(|scope| {
            let real_target = target(&real);
            let wrong_target = target(&wrong);
            let mut wrong_key = target(&real);
            wrong_key.key = Some(wrong.key());
            let a = scope.spawn(move || read(&real_target));
            let b = scope.spawn(move || read(&wrong_target));
            let c = scope.spawn(move || read(&wrong_key));
            let started = Instant::now();
            while Arc::strong_count(&opened.exchange) < 5 {
                assert!(started.elapsed() < HANG, "every reader joins the exchange");
                aterm_uds::deadline::poll_yield();
            }
            opened.exchange.tell(Exchanged::Refused(ProvedRefusal {
                ended: matches!(error, FleetError::UnknownOp(_)),
                why: Unreached::Handshake(at, error),
                peer: NodeProof {
                    node: real.node().to_owned(),
                    key: real.key(),
                },
            }));
            (a.join().unwrap(), b.join().unwrap(), c.join().unwrap())
        });
        assert!(matches!(a, Err(Unreached::Handshake(got, _)) if got == at));
        assert!(matches!(b, Err(Unreached::OtherNode { at: got, .. }) if got == at));
        assert!(matches!(c, Err(Unreached::Impostor { at: got, .. }) if got == at));
    }
}

/// A companion lookup that cannot acquire capacity cannot discard a lookup already running.
/// The admission witness releases that lookup only after the companion was refused, avoiding a
/// race where the first lookup returns its permit before the second one asks for it.
#[test]
fn a_partly_admitted_lookup_keeps_its_result_and_its_limit() {
    static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
    for found in [true, false] {
        let held: Vec<_> = (0..RESOLVER_THREADS_MAX - 1)
            .map(|_| ResolverPermit::take(&IN_FLIGHT).unwrap())
            .collect();
        let (release, released) = mpsc::channel();
        let released = Arc::new(Mutex::new(released));
        let (refused, saw_refusal) = mpsc::channel();
        let (retired, retirement) = mpsc::channel();
        let result = std::thread::scope(|scope| {
            let lookup = scope.spawn(|| {
                resolve_with_admission(
                    "lab-mini",
                    7100,
                    HANG,
                    move |_, port| {
                        released.lock().unwrap().recv_timeout(HANG).unwrap();
                        let _ = retired.send(());
                        if found {
                            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))]
                        } else {
                            Vec::new()
                        }
                    },
                    || {
                        let permit = ResolverPermit::take(&IN_FLIGHT);
                        if permit.is_none() {
                            refused.send(()).unwrap();
                        }
                        permit
                    },
                )
            });
            saw_refusal
                .recv_timeout(HANG)
                .expect("the companion has no permit");
            assert_eq!(IN_FLIGHT.load(Ordering::Acquire), RESOLVER_THREADS_MAX);
            release.send(()).unwrap();
            lookup.join().unwrap()
        });
        if found {
            assert!(
                matches!(result, Lookup::Found(ref addresses) if addresses == &[SocketAddr::from((Ipv4Addr::LOCALHOST, 7100))])
            );
        } else {
            assert!(
                matches!(result, Lookup::Unstarted(ref error) if error.kind() == io::ErrorKind::WouldBlock)
            );
        }
        retirement.recv_timeout(HANG).unwrap();
        drop(held);
        let started = Instant::now();
        while IN_FLIGHT.load(Ordering::Acquire) != 0 {
            assert!(
                started.elapsed() < HANG,
                "the admitted lookup returns its permit"
            );
            aterm_uds::deadline::poll_yield();
        }
    }
    let _held: Vec<_> = (0..RESOLVER_THREADS_MAX)
        .map(|_| ResolverPermit::take(&IN_FLIGHT).unwrap())
        .collect();
    let result = resolve_with_admission(
        "lab-mini",
        7100,
        HANG,
        |_, _| panic!("no lookup admitted"),
        || ResolverPermit::take(&IN_FLIGHT),
    );
    assert!(
        matches!(result, Lookup::Unstarted(ref error) if error.kind() == io::ErrorKind::WouldBlock)
    );
}

/// The companion is held independently of the resolver thread's scheduling. A one-minute
/// detector is the only wait bound; the operation's larger budget lets the test distinguish a
/// result returned while the companion is held from one returned only after its detector fires.
struct LookupGate {
    release: mpsc::Sender<()>,
    began: mpsc::Receiver<()>,
    returned: mpsc::Receiver<()>,
    timed_out: Arc<AtomicBool>,
}

impl LookupGate {
    fn finish(&self) {
        let _ = self.release.send(());
        self.returned
            .recv_timeout(HANG)
            .expect("the released lookup returns");
    }
}

impl Drop for LookupGate {
    fn drop(&mut self) {
        // A failed assertion must not strand the resolver's permit.
        let _ = self.release.send(());
    }
}

/// The names a [`split_lookup`] resolver was asked to look up, in the order it was asked.
type Asked = Arc<Mutex<Vec<String>>>;

fn split_lookup(
    first: Vec<SocketAddr>,
    companion: Vec<SocketAddr>,
) -> (
    impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
    LookupGate,
    Asked,
) {
    let (release, released) = mpsc::channel();
    let released = Arc::new(Mutex::new(released));
    let (began, saw_begin) = mpsc::channel();
    let (returned, saw_return) = mpsc::channel();
    let timed_out = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let resolver = {
        let timed_out = Arc::clone(&timed_out);
        let calls = Arc::clone(&calls);
        move |name: &str, _: u16| {
            calls.lock().unwrap().push(name.to_owned());
            if name == "split-peer" {
                return first.clone();
            }
            assert_eq!(name, "split-peer.local");
            let _ = began.send(());
            if released.lock().unwrap().recv_timeout(HANG).is_err() {
                timed_out.store(true, Ordering::Release);
            }
            let _ = returned.send(());
            companion.clone()
        }
    };
    (
        resolver,
        LookupGate {
            release,
            began: saw_begin,
            returned: saw_return,
            timed_out,
        },
        calls,
    )
}

fn split_target(who: &NodeIdentity) -> Target {
    Target {
        host: "split-peer".to_owned(),
        addr: None,
        port: 7100,
        node: Some(who.node().to_owned()),
        key: Some(who.key()),
    }
}

fn asks_bound_target(
    op: FleetOp,
    target: &Target,
    me: &NodeIdentity,
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
) -> Result<SocketAddr, Unreached> {
    let deadline = (Instant::now() + 2 * HANG, 2 * HANG);
    if op == FleetOp::Drive {
        reach_node_with(
            target,
            &Keyring::default(),
            (&secret(6), me),
            op,
            deadline,
            resolver,
        )
        .and_then(|(reached, node)| {
            let mut stream = driven(reached, node, deadline)?;
            stream.write_all(b"@s-1 status\n").unwrap();
            let mut answer = String::new();
            io::BufRead::read_line(&mut io::BufReader::new(&mut stream), &mut answer).unwrap();
            assert_eq!(answer, "OK reached\n");
            Ok(stream.addr)
        })
    } else {
        read_one_with(
            target,
            &Keyring::default(),
            (&secret(6), Some(me)),
            deadline,
            "survey",
            &HeldAddrs::default(),
            resolver,
        )
        .map(|survey| {
            assert!(survey.proven);
            assert_eq!(survey.node.as_deref(), target.node.as_deref());
            survey.addr
        })
    }
}

#[test]
fn a_companion_lookup_keeps_the_bound_target_after_the_first_batch_is_wrong() {
    let me = identity(&node_of(0xe1), 0xe1);
    let wanted = identity(&node_of(0xe2), 0xe2);
    let wrong = identity(&node_of(0xe3), 0xe3);
    for (op, correct) in [FleetOp::Drive, FleetOp::Survey]
        .into_iter()
        .flat_map(|op| [(op, true), (op, false)])
    {
        let second = if correct {
            wanted.clone()
        } else {
            identity(&node_of(0xe6), 0xe6)
        };
        let target_peer = Peer::start(second, SERVES);
        let (proved, saw_proof) = mpsc::channel();
        let wrong_peer = Peer::start_notifying(wrong.clone(), SERVES, Some(proved));
        let (resolver, gate, calls) = split_lookup(
            vec![wrong_peer.addr, wrong_peer.addr],
            vec![wrong_peer.addr, target_peer.addr, target_peer.addr],
        );
        let result = std::thread::scope(|scope| {
            let target = split_target(&wanted);
            let me = &me;
            let ask = scope.spawn(move || asks_bound_target(op, &target, me, resolver));
            gate.began.recv_timeout(HANG).expect("companion started");
            saw_proof
                .recv_timeout(HANG)
                .expect("the first batch's peer completed its proof");
            gate.finish();
            ask.join().unwrap()
        });
        if correct {
            assert_eq!(result.unwrap(), target_peer.addr);
        } else {
            assert!(
                matches!(result, Err(Unreached::OtherNode { at, said, want })
                if at == wrong_peer.addr && said == wrong.node() && want == wanted.node())
            );
        }
        assert_eq!(wrong_peer.asked.load(Ordering::Acquire), 0);
        assert_eq!(
            target_peer.asked.load(Ordering::Acquire),
            usize::from(correct)
        );
        let mut called = calls.lock().unwrap().clone();
        called.sort();
        assert_eq!(called, ["split-peer", "split-peer.local"]);
    }
}

#[test]
fn a_walk_deduplicates_across_lookup_batches_and_keeps_its_first_failure() {
    let first = SocketAddr::from((Ipv4Addr::LOCALHOST, 7111));
    let second = SocketAddr::from((Ipv4Addr::LOCALHOST, 7222));
    let (resolver, gate, calls) = split_lookup(vec![first, first], vec![first, second, second]);
    let mut attempted = Vec::new();
    let result = walk(
        ("split-peer", None, 7100),
        (Instant::now() + 2 * HANG, 2 * HANG),
        (&[], resolver),
        |at| -> AtAddr<()> {
            attempted.push(at);
            if at == first {
                let _ = gate.release.send(());
            }
            AtAddr::Passed(Unreached::Connect(
                at,
                io::ErrorKind::ConnectionRefused.into(),
            ))
        },
    );
    gate.finish();
    assert_eq!(attempted, [first, second]);
    assert!(matches!(result, Err(Unreached::Connect(at, _)) if at == first));
    let mut called = calls.lock().unwrap().clone();
    called.sort();
    assert_eq!(called, ["split-peer", "split-peer.local"]);
}

#[test]
fn a_completed_target_does_not_wait_for_its_companion_lookup() {
    let me = identity(&node_of(0xe4), 0xe4);
    let wanted = identity(&node_of(0xe5), 0xe5);
    for serves in [SERVES, &[FleetOp::Survey][..]] {
        let target_peer = Peer::start(wanted.clone(), serves);
        let (resolver, gate, _) = split_lookup(vec![target_peer.addr], Vec::new());
        let (finished, result) = mpsc::channel();
        std::thread::scope(|scope| {
            let target = split_target(&wanted);
            let me = &me;
            let ask = scope.spawn(move || {
                let answer = asks_bound_target(FleetOp::Drive, &target, me, resolver);
                let _ = finished.send(answer);
            });
            gate.began.recv_timeout(HANG).expect("companion started");
            let completed_while_held = result.recv_timeout(HANG);
            let gate_timed_out = gate.timed_out.load(Ordering::Acquire);
            gate.finish();
            ask.join().unwrap();
            assert!(
                !gate_timed_out,
                "the result must precede the companion's hang detector"
            );
            let answer = completed_while_held.expect("target returns while companion is held");
            if serves == SERVES {
                assert_eq!(answer.unwrap(), target_peer.addr);
            } else {
                assert!(
                    matches!(answer, Err(Unreached::Handshake(at, FleetError::UnknownOp(op)))
                    if at == target_peer.addr && op == "drive")
                );
                assert_eq!(target_peer.asked.load(Ordering::Acquire), 0);
            }
        });
    }
}

#[test]
fn an_address_snapshot_returns_the_first_batch_while_the_companion_is_held() {
    let first = SocketAddr::from((Ipv4Addr::LOCALHOST, 7111));
    let later = SocketAddr::from((Ipv4Addr::LOCALHOST, 7222));
    let (resolver, gate, _) = split_lookup(vec![first], vec![later]);
    let (finished, result) = mpsc::channel();
    std::thread::scope(|scope| {
        let lookup = scope.spawn(move || {
            let answer = resolve_with_admission("split-peer", 7100, 2 * HANG, resolver, || {
                ResolverPermit::take(&RESOLVERS_IN_FLIGHT)
            });
            let _ = finished.send(answer);
        });
        gate.began.recv_timeout(HANG).expect("companion started");
        let completed_while_held = result.recv_timeout(HANG);
        let gate_timed_out = gate.timed_out.load(Ordering::Acquire);
        gate.finish();
        lookup.join().unwrap();
        assert!(!gate_timed_out);
        assert!(
            matches!(completed_while_held.unwrap(), Lookup::Found(addresses) if addresses == [first])
        );
    });
}

#[test]
fn lookup_batches_keep_empty_results_admission_errors_and_literal_deadlines() {
    static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, 7111));
    for found in [true, false] {
        // Admit the first lookup but refuse its companion independently of worker scheduling.
        let mut available = true;
        let mut batches = resolve_batches_with_admission(
            "split-peer",
            7100,
            Instant::now() + HANG,
            move |_, _| if found { vec![at] } else { Vec::new() },
            || {
                std::mem::replace(&mut available, false)
                    .then(|| ResolverPermit::take(&IN_FLIGHT))
                    .flatten()
            },
        );
        if found {
            assert!(matches!(batches.next(), Some(Lookup::Found(addresses)) if addresses == [at]));
        }
        assert!(matches!(batches.next(), Some(Lookup::Unstarted(error))
            if error.kind() == io::ErrorKind::WouldBlock));
        assert!(batches.next().is_none());
    }
    let mut empty = resolve_batches_with_admission(
        "split-peer",
        7100,
        Instant::now() + HANG,
        |_, _| Vec::new(),
        || ResolverPermit::take(&IN_FLIGHT),
    );
    assert!(empty.next().is_none());
    let mut nonempty = resolve_batches_with_admission(
        "split-peer",
        7100,
        Instant::now() + HANG,
        move |name, _| {
            if name == "split-peer.local" {
                vec![at]
            } else {
                Vec::new()
            }
        },
        || ResolverPermit::take(&IN_FLIGHT),
    );
    assert!(matches!(nonempty.next(), Some(Lookup::Found(addresses)) if addresses == [at]));
    assert!(nonempty.next().is_none());
    let past = Instant::now();
    let mut expired = resolve_batches_with_admission(
        "split-peer",
        7100,
        past,
        |_, _| panic!("an expired lookup must not start"),
        || panic!("an expired lookup needs no permit"),
    );
    assert!(matches!(expired.next(), Some(Lookup::Late)));
    assert!(expired.next().is_none());
    let mut literal = resolve_batches_with_admission(
        "127.0.0.1",
        7111,
        past,
        |_, _| panic!("a literal needs no resolver"),
        || panic!("a literal needs no permit"),
    );
    assert!(matches!(literal.next(), Some(Lookup::Found(addresses)) if addresses == [at]));
    assert!(literal.next().is_none());
}
