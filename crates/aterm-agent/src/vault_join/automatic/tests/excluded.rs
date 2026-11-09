// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::commons::bootstrap::Bootstrap;
use aterm_secrets::members::bundle::Handed;

/// The node's automatic worker on a member the current Commons epochs leave out of control: its
/// snapshot is [`Local::of`]'s, on the member's home ALab LAN (the test network).
struct Excluded {
    paths: Paths,
    now_ms: u64,
    waiting: Sender<Instant>,
    read_gate: Mutex<Option<Gate>>,
    reads: AtomicUsize,
    clock: Instant,
    advance: Mutex<Duration>,
}

impl Backend for Excluded {
    fn read(&self) -> Result<Option<(Local, String)>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.read_gate.lock_or_recover().take() {
            gate.wait();
        }
        Ok(Local::of(&self.paths)?.map(|local| (local.with_automatic_lan([]), "home".to_owned())))
    }

    fn now(&self) -> Instant {
        self.clock + *self.advance.lock_or_recover()
    }

    fn refuses(&self, proving: &desk::Proving<'_>, _peer: IpAddr) -> Option<String> {
        client::refuses_before_possession_at_ms(
            &self.paths,
            proving,
            client::Book::default(),
            self.now_ms,
        )
    }

    fn prove(&self, proving: &desk::Proving<'_>, _peer: IpAddr) -> Result<SecretBytes> {
        assert!(proving.lan, "a proof from the home ALab LAN");
        client::prove_at_ms(&self.paths, proving, client::Book::default(), self.now_ms)
    }

    fn waiting(&self, until: Instant) {
        let _ = self.waiting.send(until);
    }
}

/// A member whose Commons control the current epochs took away still lets a Mac on its home ALab
/// LAN in with no window open: the worker's snapshot is the member's desk, which its membership
/// log seat alone makes (owner, 2026-11-02), and the Mac's proof on the node's port gets the log
/// bundle the Mac writes and is in by.
/// The owned component path includes the real worker publication, public generation notifier,
/// loopback READY receiver and fresh solicitation before that proof. The home network remains
/// controlled fixture input; this is not ordinary app startup, subscriber dispatch or physical LAN.
///
/// NEGATIVE CONTROL (in-test oracle): the old desk stood on a Commons bootstrap, which this member
/// has none of (asserted below), so the old worker published no snapshot and refused the proof
/// "automatic LAN admission is unavailable on this Mac". By hand: the seated arm of `Local::of`
/// answering `Ok(None)` fails here with that refusal.
#[test]
fn a_member_commons_control_leaves_out_lets_a_home_lan_mac_in_with_no_window() {
    use aterm_secrets::client::MemberArrivals;

    let root = aterm_tempfile::tempdir().unwrap();
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    assert!(
        Bootstrap::cached(&fixture.member).unwrap().is_none(),
        "the oracle: the old desk had nothing to stand on here"
    );
    let trust = Bootstrap::required(&fixture.admin).unwrap().trust();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let member_node =
        aterm_net::fleet::NodeIdentity::from_seed(&"11".repeat(16), &[11; 32]).unwrap();
    let (responder, mut arrivals, bound) =
        crate::lan_members::Responder::loopback_for_test(address.port(), member_node.key())
            .unwrap();
    let advertiser = Arc::new(
        crate::node_service::AlabTcpAdvertiser::new("joined-loopback".to_owned())
            .holding_responder_for_test(responder),
    );
    assert_eq!(bound.recv_timeout(HANG).unwrap(), 1);
    let await_arrival = |arrivals: &mut crate::lan_members::Arrivals| {
        let until = Instant::now() + HANG;
        while !MemberArrivals::poll(arrivals).unwrap() {
            assert!(Instant::now() < until, "the owned member emitted no READY");
            aterm_uds::ready::any_until(&MemberArrivals::fds(arrivals), until).unwrap();
        }
        assert!(
            !MemberArrivals::poll(arrivals).unwrap(),
            "the same READY is consumed once"
        );
    };
    // An address can be advertised before its admission read completes. Consume that initial
    // real bind hint so only the worker's later capability edge can wake this receiver again.
    await_arrival(&mut arrivals);

    struct Announcing {
        admission: crate::node_service::AdmissionAnnouncer,
        advertiser: Arc<crate::node_service::AlabTcpAdvertiser>,
        service: Option<AutomaticJoins>,
        release: Option<Sender<()>>,
    }
    impl Drop for Announcing {
        fn drop(&mut self) {
            self.admission.close();
            self.advertiser.stop();
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
            drop(self.service.take());
        }
    }

    let (gate, reading, release) = Gate::pair();
    let (waiting, ready) = mpsc::channel();
    let backend = Arc::new(Excluded {
        paths: fixture.member.clone(),
        now_ms: aterm_time::unix_millis().unwrap(),
        waiting,
        read_gate: Mutex::new(Some(gate)),
        reads: AtomicUsize::new(0),
        clock: Instant::now(),
        advance: Mutex::new(Duration::ZERO),
    });
    let mut announcing = Announcing {
        admission: advertiser.admission_announcer(address.port()),
        advertiser,
        service: None,
        release: Some(release),
    };
    let published = Arc::new(AtomicUsize::new(0));
    let notify = announcing.admission.notifier();
    let accepted = Arc::clone(&published);
    announcing.service = Some(
        AutomaticJoins::with_backend(
            backend.clone(),
            Arc::new(move || {
                notify();
                accepted.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap(),
    );
    reading.recv_timeout(HANG).unwrap();
    let handle = announcing.service.as_ref().unwrap().handle();
    std::thread::scope(|scope| {
        let mut stream = TcpStream::connect_timeout(&address, HANG).unwrap();
        stream.set_read_timeout(Some(HANG)).unwrap();
        stream.set_write_timeout(Some(HANG)).unwrap();
        let refused = scope.spawn(|| {
            let (mut stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            stream.set_read_timeout(Some(HANG)).unwrap();
            stream.set_write_timeout(Some(HANG)).unwrap();
            handle.serve(&mut stream, "192.168.1.40".parse().unwrap())
        });
        assert!(
            joiner::greeting(&mut stream)
                .unwrap_err()
                .to_string()
                .contains(UNAVAILABLE)
        );
        drop(stream);
        assert!(refused.join().unwrap().is_err());
    });
    assert_eq!(published.load(Ordering::SeqCst), 0);
    assert!(!MemberArrivals::poll(&mut arrivals).unwrap());
    announcing.release.take().unwrap().send(()).unwrap();
    let healthy_until = ready.recv_timeout(HANG).unwrap();
    assert_eq!(published.load(Ordering::SeqCst), 1);
    assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
    await_arrival(&mut arrivals);
    // Hearing READY supplies no address or admission: solicit again, then perform the
    // existing real signed member proof against the listener named by that fresh response.
    let hints = arrivals.probe_for_test(HANG).unwrap();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].address, address);
    assert_eq!(hints[0].key, member_node.key());

    {
        let _state = handle.shared.state.lock_or_recover();
        let left = healthy_until.saturating_duration_since(backend.now());
        *backend.advance.lock_or_recover() += left;
        handle.shared.changed.notify_all();
    }
    ready.recv_timeout(HANG).unwrap();
    assert_eq!(backend.reads.load(Ordering::SeqCst), 2);
    assert_eq!(
        published.load(Ordering::SeqCst),
        1,
        "healthy renewal invokes no capability callback"
    );

    let fresh = Paths::for_home(&root.path().join("fresh"));
    client::machine_identity(&fresh).unwrap();
    let device = Device::open(&fresh).unwrap().unwrap();
    let node = aterm_net::fleet::NodeIdentity::from_seed(&"31".repeat(16), &[31; 32]).unwrap();
    let proof_address = hints[0].address;
    let served = std::thread::spawn(move || {
        let (mut stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        stream.set_read_timeout(Some(HANG)).unwrap();
        stream.set_write_timeout(Some(HANG)).unwrap();
        handle.serve(&mut stream, "192.168.1.40".parse().unwrap())
    });
    let mut stream = TcpStream::connect_timeout(&proof_address, HANG).unwrap();
    stream.set_read_timeout(Some(HANG)).unwrap();
    stream.set_write_timeout(Some(HANG)).unwrap();
    let mut sent = false;
    let bundle = (|| {
        let greeting = joiner::greeting(&mut stream)?;
        let joining = Joining {
            records_bound: aterm_secrets::crossing::RecordsBound::uncrossed(),
            device: &device,
            node: *node.key().bytes(),
            name: "fresh".to_owned(),
            model: "test".to_owned(),
        };
        let sign_node = |message: &[u8]| node.sign_record(desk::PROVE_RECORD, message);
        joiner::prove(&mut stream, &greeting, &joining, &sign_node, &mut sent)
    })();
    drop(stream);
    let served = served.join().unwrap();
    let bundle = bundle.expect("the excluded member's worker lets the home LAN Mac in");
    served.expect("the worker served the proof");
    assert!(matches!(bundle.handed, Handed::Log { .. }));
    client::write_from_log_bundle(&fresh, &bundle).unwrap();
    assert!(matches!(
        client::status(&fresh).unwrap().membership,
        client::Membership::In { .. }
    ));
    assert_eq!(
        std::fs::read(fresh.home.join(".secrets/shared")).unwrap(),
        b"everyone"
    );
    let actor = device
        .request(trust.network, trust.authority)
        .unwrap()
        .binding()
        .actor();
    assert!(
        client::load_records(&fresh)
            .unwrap()
            .log()
            .unwrap()
            .membership()
            .contains(&actor)
    );
    // An accepted worker publication can still invoke its retained notifier after generation
    // closure. Exercise that lifetime without inferring packet absence from a one-shot poll.
    announcing.admission.close();
    let retained = announcing.admission.notifier();
    let advertiser = Arc::downgrade(&announcing.advertiser);
    let handle = announcing.service.as_ref().unwrap().handle();
    handle.refresh();
    ready.recv_timeout(HANG).unwrap();
    assert_eq!(backend.reads.load(Ordering::SeqCst), 3);
    assert_eq!(published.load(Ordering::SeqCst), 2);
    drop(announcing);
    assert!(
        advertiser.upgrade().is_none(),
        "a retained callback owns no advertiser"
    );
    retained();
    fixture.server.stop();
}
