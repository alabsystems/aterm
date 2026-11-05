// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::commons::bootstrap::Bootstrap;
use aterm_secrets::members::bundle::Handed;

struct Member {
    paths: Paths,
    now_ms: u64,
    waiting: Sender<Instant>,
    decisions: Mutex<Vec<bool>>,
}

impl Backend for Member {
    fn read(&self) -> Result<Option<(Local, String)>> {
        Ok(Local::of(&self.paths)?.map(|local| (local.with_home(None), "away".to_owned())))
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
        self.decisions.lock_or_recover().push(proving.lan);
        client::prove_at_ms(&self.paths, proving, client::Book::default(), self.now_ms)
    }

    fn waiting(&self, until: Instant) {
        let _ = self.waiting.send(until);
    }
}

/// Real signed vault/log, and an admitted device whose local vault files still need recovery.
/// The setup's explicit admission is fixture construction; the wire exchange uses only proof.
struct Recovery {
    service: AutomaticJoins,
    server: aterm_secrets::server::Running,
    paths: Paths,
    member: Arc<Member>,
    device: Device,
    node: aterm_net::fleet::NodeIdentity,
    trust: Trust,
    admit: aterm_secrets::membership_log::RecordId,
    _root: aterm_tempfile::TempDir,
}

impl Recovery {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = aterm_secrets::server::start(
            listener,
            &aterm_secrets::server::Options {
                listen: address,
                store: root.path().join("server"),
                advertise: None,
                pins: Vec::new(),
                hold: HANG,
                keep_awake: false,
            },
        )
        .unwrap();
        let member_paths = Paths::for_home(&root.path().join("member"));
        client::create(
            &member_paths,
            &format!("http://{address}/v1/recovery"),
            None,
            Some("member"),
        )
        .unwrap();
        let admin = client::admin_open(&member_paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &aterm_secrets::text::Audience::Everyone,
            "recovery-token",
            b"fixture-value",
        )
        .unwrap();
        client::admin_publish(&member_paths, &admin, &doc).unwrap();
        client::seed_vault_genesis(&member_paths, &root.path().join("genesis")).unwrap();
        let trust = Bootstrap::required(&member_paths).unwrap().trust();
        let paths = Paths::for_home(&root.path().join("recovering"));
        client::machine_identity(&paths).unwrap();
        let device = Device::open(&paths).unwrap().unwrap();
        let request = device.request(trust.network, trust.authority).unwrap();
        let node = aterm_net::fleet::NodeIdentity::from_seed(&"07".repeat(16), &[7; 32]).unwrap();
        let now_ms = aterm_time::unix_millis().unwrap();
        let admitted = client::admit_by_phrase(
            &member_paths,
            &request,
            *node.key().bytes(),
            "recovering",
            now_ms,
        )
        .unwrap();
        assert!(matches!(admitted, client::ByLog::Admitted(_)));
        let store = client::load_records(&member_paths).unwrap();
        let admit = store
            .log()
            .unwrap()
            .membership()
            .get(&request.binding().actor())
            .unwrap()
            .entry;
        let (waiting, ready) = mpsc::channel();
        let member = Arc::new(Member {
            paths: member_paths,
            now_ms,
            waiting,
            decisions: Mutex::new(Vec::new()),
        });
        let service = AutomaticJoins::with_backend(member.clone()).unwrap();
        ready.recv_timeout(HANG).unwrap();
        Self {
            _root: root,
            server,
            paths,
            member,
            service,
            device,
            node,
            trust,
            admit,
        }
    }

    fn prove(
        &self,
        device: &Device,
        node: &aterm_net::fleet::NodeIdentity,
    ) -> (Result<aterm_secrets::members::Bundle>, bool) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let handle = self.service.handle();
        let served = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(HANG)).unwrap();
            stream.set_write_timeout(Some(HANG)).unwrap();
            handle.serve(&mut stream, "172.20.10.40".parse().unwrap())
        });
        let mut stream = TcpStream::connect_timeout(&address, HANG).unwrap();
        stream.set_read_timeout(Some(HANG)).unwrap();
        stream.set_write_timeout(Some(HANG)).unwrap();
        let mut sent = false;
        let result = (|| {
            let greeting = joiner::greeting(&mut stream)?;
            let joining = Joining {
                records_bound: aterm_secrets::crossing::RecordsBound::uncrossed(),
                device,
                node: *node.key().bytes(),
                name: "recovering".to_owned(),
                model: "test".to_owned(),
            };
            let sign_node = |message: &[u8]| node.sign_record(desk::PROVE_RECORD, message);
            joiner::prove(&mut stream, &greeting, &joining, &sign_node, &mut sent)
        })();
        drop(stream);
        let served = served.join().unwrap();
        assert_eq!(served.is_ok(), result.is_ok());
        (result, sent)
    }
}

#[test]
fn an_existing_seat_recovers_by_proof_on_a_headless_members_port_off_home() {
    let f = Recovery::new();
    let request = f
        .device
        .request(f.trust.network, f.trust.authority)
        .unwrap();
    let fresh = std::cell::Cell::new(false);
    let proving = desk::Proving {
        reader: aterm_secrets::members::bundle::Reader::BoundedLog(
            aterm_secrets::crossing::RecordsBound::uncrossed(),
        ),
        request: &request,
        node: *f.node.key().bytes(),
        name: "recovering",
        fleet: false,
        lan: false,
        lab: false,
        fresh: &fresh,
    };
    assert!(
        f.member
            .refuses(&proving, "172.20.10.40".parse().unwrap())
            .is_none()
    );
    let (result, sent) = f.prove(&f.device, &f.node);
    f.server.stop();
    let bundle = result.expect("the existing seat's real proof recovers its log bundle off home");
    assert!(sent, "the existing seat answered the possession challenge");
    assert!(
        matches!(bundle.handed, Handed::Log { admit, source: Some(source), .. }
        if admit == f.admit && source == aterm_secrets::crossing::RecordsBound::uncrossed())
    );
    assert_eq!(bundle.allowed.binding(), request.binding());
    client::write_from_log_bundle(&f.paths, &bundle).unwrap();
    assert_eq!(
        std::fs::read(f.paths.home.join(".secrets/recovery-token")).unwrap(),
        b"fixture-value"
    );
    assert!(matches!(
        client::status(&f.paths).unwrap().membership,
        client::Membership::In { .. }
    ));
    assert_eq!(*f.member.decisions.lock_or_recover(), [false]);
}

#[test]
fn a_new_off_home_mac_is_refused_before_possession_without_changing_the_log() {
    let f = Recovery::new();
    let paths = Paths::for_home(&f._root.path().join("unknown"));
    client::machine_identity(&paths).unwrap();
    let device = Device::open(&paths).unwrap().unwrap();
    let node = aterm_net::fleet::NodeIdentity::from_seed(&"08".repeat(16), &[8; 32]).unwrap();
    let before = std::fs::read(f.member.paths.records_file()).unwrap();
    let (result, sent) = f.prove(&device, &node);
    f.server.stop();
    assert!(result.is_err());
    assert!(
        !sent,
        "a new off-home identity is refused before possession"
    );
    assert!(f.member.decisions.lock_or_recover().is_empty());
    assert_eq!(
        std::fs::read(f.member.paths.records_file()).unwrap(),
        before
    );
}
