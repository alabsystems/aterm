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
}

impl Backend for Excluded {
    fn read(&self) -> Result<Option<(Local, String)>> {
        Ok(Local::of(&self.paths)?.map(|local| (local.with_automatic_lan([]), "home".to_owned())))
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
///
/// NEGATIVE CONTROL (in-test oracle): the old desk stood on a Commons bootstrap, which this member
/// has none of (asserted below), so the old worker published no snapshot and refused the proof
/// "automatic LAN admission is unavailable on this Mac". By hand: the seated arm of `Local::of`
/// answering `Ok(None)` fails here with that refusal.
#[test]
fn a_member_commons_control_leaves_out_lets_a_home_lan_mac_in_with_no_window() {
    let root = aterm_tempfile::tempdir().unwrap();
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    assert!(
        Bootstrap::cached(&fixture.member).unwrap().is_none(),
        "the oracle: the old desk had nothing to stand on here"
    );
    let trust = Bootstrap::required(&fixture.admin).unwrap().trust();
    let (waiting, ready) = mpsc::channel();
    let service = AutomaticJoins::with_backend(Arc::new(Excluded {
        paths: fixture.member.clone(),
        now_ms: aterm_time::unix_millis().unwrap(),
        waiting,
    }))
    .unwrap();
    ready.recv_timeout(HANG).unwrap();

    let fresh = Paths::for_home(&root.path().join("fresh"));
    client::machine_identity(&fresh).unwrap();
    let device = Device::open(&fresh).unwrap().unwrap();
    let node = aterm_net::fleet::NodeIdentity::from_seed(&"31".repeat(16), &[31; 32]).unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let handle = service.handle();
    let served = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(HANG)).unwrap();
        stream.set_write_timeout(Some(HANG)).unwrap();
        handle.serve(&mut stream, "192.168.1.40".parse().unwrap())
    });
    let mut stream = TcpStream::connect_timeout(&address, HANG).unwrap();
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
    drop(service);
    fixture.server.stop();
}
