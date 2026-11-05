// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client;
use aterm_secrets::commons::{bootstrap::Device, journal::Trust};
use aterm_secrets::members::joiner::{self, Joining};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

const HANG: Duration = Duration::from_secs(60);

mod excluded;
mod recovery;
mod recovery_invalidation;
mod request_policy;
const TRUST: Trust = Trust {
    network: [1; 16],
    authority: [2; 32],
    genesis: [3; 32],
};

struct Gate {
    entered: Sender<()>,
    release: Receiver<()>,
}

impl Gate {
    fn pair() -> (Self, Receiver<()>, Sender<()>) {
        let (entered, heard) = mpsc::channel();
        let (release, released) = mpsc::channel();
        (
            Self {
                entered,
                release: released,
            },
            heard,
            release,
        )
    }

    fn wait(self) {
        self.entered.send(()).unwrap();
        self.release
            .recv_timeout(HANG)
            .expect("release blocked work");
    }
}

#[derive(Clone)]
struct Place {
    network: &'static str,
    home: bool,
}

struct Fake {
    root: std::path::PathBuf,
    advance: Mutex<Duration>,
    frozen: Mutex<Option<Instant>>,
    place: Mutex<Option<Place>>,
    read_gate: Mutex<Option<Gate>>,
    refusal_gate: Mutex<Option<Gate>>,
    fail_read: AtomicBool,
    waiting: Sender<Instant>,
    reads: AtomicUsize,
    /// Each proof this member decided, by whether it came from its home ALab LAN.
    proved: Mutex<Vec<bool>>,
    refusals: AtomicUsize,
}

fn device(root: &std::path::Path, name: &str) -> Device {
    let paths = Paths::for_home(&root.join(name));
    client::machine_identity(&paths).unwrap();
    Device::open(&paths).unwrap().unwrap()
}

impl Backend for Fake {
    fn read(&self) -> Result<Option<(Local, String)>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let place = self.place.lock_or_recover().clone();
        let gate = self.read_gate.lock_or_recover().take();
        if let Some(gate) = gate {
            gate.wait();
        }
        if self.fail_read.swap(false, Ordering::SeqCst) {
            return Err(Error::new("membership temporarily unreadable"));
        }
        Ok(place.map(|place| {
            let local = Local::with(device(&self.root, "member"), TRUST, true, "member")
                .unwrap()
                .with_automatic_lan([]);
            let local = if place.home {
                local
            } else {
                local.with_home(None)
            };
            (local, place.network.to_owned())
        }))
    }

    fn now(&self) -> Instant {
        let frozen = *self.frozen.lock_or_recover();
        frozen.unwrap_or_else(Instant::now) + *self.advance.lock_or_recover()
    }

    fn prove(&self, proving: &desk::Proving<'_>, _peer: IpAddr) -> Result<SecretBytes> {
        self.proved.lock_or_recover().push(proving.lan);
        Err(Error::new("decided by the test's member"))
    }

    fn refuses(&self, _proving: &desk::Proving<'_>, _peer: IpAddr) -> Option<String> {
        self.refusals.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.refusal_gate.lock_or_recover().take() {
            gate.wait();
        }
        None
    }

    fn waiting(&self, until: Instant) {
        self.waiting.send(until).unwrap();
    }
}

struct Fixture {
    service: Option<AutomaticJoins>,
    fake: Arc<Fake>,
    waits: Receiver<Instant>,
    _root: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new(place: Option<Place>) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let (waiting, waits) = mpsc::channel();
        let fake = Arc::new(Fake {
            root: root.path().to_owned(),
            advance: Mutex::new(Duration::ZERO),
            frozen: Mutex::new(None),
            place: Mutex::new(place),
            read_gate: Mutex::new(None),
            refusal_gate: Mutex::new(None),
            fail_read: AtomicBool::new(false),
            waiting,
            reads: AtomicUsize::new(0),
            proved: Mutex::new(Vec::new()),
            refusals: AtomicUsize::new(0),
        });
        let service = AutomaticJoins::with_backend(fake.clone()).unwrap();
        // This observes completion of the initial read, without a sleep.
        waits.recv_timeout(HANG).unwrap();
        Self {
            service: Some(service),
            fake,
            waits,
            _root: root,
        }
    }

    fn home() -> Self {
        Self::new(Some(Place {
            network: "home-a",
            home: true,
        }))
    }

    fn handle(&self) -> AutomaticJoinHandle {
        self.service.as_ref().unwrap().handle()
    }

    fn ready_on(&self, network: &str) -> Arc<Mutex<Desk>> {
        let handle = self.handle();
        let end = Instant::now() + HANG;
        let mut state = handle.shared.state.lock_or_recover();
        loop {
            if let Some(snapshot) = state.snapshot.as_ref()
                && snapshot.network == network
                && state.valid_until.is_some()
            {
                return Arc::clone(&snapshot.desk);
            }
            let left = end.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "worker never published {network}");
            state = handle.shared.changed.wait_timeout(state, left).unwrap().0;
        }
    }

    fn ask(&self, name: &str, wrong_member: bool) {
        self.ask_from(name, wrong_member, "192.168.1.40", !wrong_member);
    }

    fn ask_from(&self, name: &str, wrong_member: bool, peer: &str, accepted: bool) {
        let (mut stream, ended, worker) = self.connection_from(peer);
        let mut greeting = joiner::greeting(&mut stream).unwrap();
        if wrong_member {
            greeting.key[0] ^= 1;
        }
        let device = device(&self.fake.root, name);
        let joining = Joining {
            records_bound: aterm_secrets::crossing::RecordsBound::uncrossed(),
            device: &device,
            node: [7; 32],
            name: name.to_owned(),
            model: "Mac".to_owned(),
        };
        assert_eq!(
            joiner::ask(&mut stream, &greeting, &joining).is_ok(),
            accepted
        );
        assert_eq!(
            ended.recv_timeout(HANG).expect("ask handler finished"),
            accepted
        );
        worker.join().unwrap();
    }

    fn connection(&self) -> (TcpStream, Receiver<bool>, JoinHandle<()>) {
        self.connection_from("192.168.1.40")
    }

    fn connection_from(&self, peer: &str) -> (TcpStream, Receiver<bool>, JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = self.handle();
        let peer = peer.parse().unwrap();
        let (ended, end) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(HANG)).unwrap();
            stream.set_write_timeout(Some(HANG)).unwrap();
            let result = handle.serve(&mut stream, peer);
            ended.send(result.is_ok()).unwrap();
        });
        let stream = TcpStream::connect_timeout(&addr, HANG).unwrap();
        stream.set_read_timeout(Some(HANG)).unwrap();
        stream.set_write_timeout(Some(HANG)).unwrap();
        (stream, end, worker)
    }
}

fn refused(handle: &AutomaticJoinHandle, peer: &str) {
    let mut reply = std::io::Cursor::new(Vec::new());
    assert!(handle.serve(&mut reply, peer.parse().unwrap()).is_err());
    let reply = String::from_utf8(reply.into_inner()).unwrap();
    assert!(reply.starts_with("NO "), "{reply}");
    assert!(!reply.contains("ALAB member=1"), "{reply}");
}

/// A Mac on this member's home ALab LAN that proves itself on the node's anonymous channel reaches
/// the member's decision marked `lan`, with no window open: a windowless member lets new Macs in
/// as a window's desk does (`aterm_secrets::client::readmit`).
///
/// NEGATIVE CONTROL: `AutomaticJoinHandle::serve` back on `desk::serve` (no `Proof`): the desk
/// refuses the `prove` line, no decision runs, and the assert fails.
#[test]
fn a_windowless_member_decides_a_proof_from_its_home_lan() {
    let f = Fixture::home();
    let (mut stream, ended, worker) = f.connection();
    let greeting = joiner::greeting(&mut stream).unwrap();
    attempt_proof(stream, greeting, &f.fake.root);
    ended
        .recv_timeout(HANG)
        .expect("the proof's handler finished");
    worker.join().unwrap();
    assert_eq!(*f.fake.proved.lock_or_recover(), vec![true]);
}

fn attempt_proof(mut stream: TcpStream, greeting: joiner::Greeting, root: &std::path::Path) {
    use ring::signature::KeyPair as _;
    let device = device(root, "newcomer");
    let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[0x44; 32]).unwrap();
    let node: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
    let joining = Joining {
        records_bound: aterm_secrets::crossing::RecordsBound::uncrossed(),
        device: &device,
        node,
        name: "newcomer".to_owned(),
        model: "Mac".to_owned(),
    };
    let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);
    let sign_node = |message: &[u8]| -> [u8; 64] {
        pair.sign(&[prefix.as_bytes(), message].concat())
            .as_ref()
            .try_into()
            .unwrap()
    };
    let refused = joiner::prove(&mut stream, &greeting, &joining, &sign_node, &mut false);
    assert!(refused.is_err(), "this backend never admits a proof");
}

#[derive(Clone, Copy, Debug)]
enum PolicyChange {
    Refreshed,
    Moved,
    Unavailable,
    Expired,
    Stopped,
    PeriodicMove,
    PeriodicHomeLoss,
}

const POLICY_CHANGES: [PolicyChange; 7] = [
    PolicyChange::Refreshed,
    PolicyChange::Moved,
    PolicyChange::Unavailable,
    PolicyChange::Expired,
    PolicyChange::Stopped,
    PolicyChange::PeriodicMove,
    PolicyChange::PeriodicHomeLoss,
];

impl PolicyChange {
    fn apply(self, f: &mut Fixture) {
        match self {
            Self::Refreshed => {
                f.handle().refresh();
                f.ready_on("home-a");
            }
            Self::Moved => {
                *f.fake.place.lock_or_recover() = Some(Place {
                    network: "home-b",
                    home: true,
                });
                f.handle().refresh();
                f.ready_on("home-b");
            }
            Self::Unavailable => {
                f.fake.fail_read.store(true, Ordering::SeqCst);
                f.handle().refresh();
                f.waits.recv_timeout(HANG).unwrap();
            }
            Self::Expired => *f.fake.advance.lock_or_recover() += REFRESH_EVERY,
            Self::Stopped => drop(f.service.take()),
            Self::PeriodicMove | Self::PeriodicHomeLoss => {
                *f.fake.place.lock_or_recover() = Some(Place {
                    network: if matches!(self, Self::PeriodicMove) {
                        "home-b"
                    } else {
                        "home-a"
                    },
                    home: matches!(self, Self::PeriodicMove),
                });
                *f.fake.advance.lock_or_recover() += REFRESH_EVERY;
                f.handle().shared.changed.notify_all();
                f.waits.recv_timeout(HANG).unwrap();
            }
        }
    }
}

/// The greeting does not reserve policy: even a freshly republished desk cannot revive a
/// connection whose generation was invalidated. Periodic reads can replace its desk or policy
/// without a watcher notification, and an expired snapshot cannot authorize a proof either.
#[test]
fn an_invalidated_proof_does_not_start_the_vault_preflight() {
    for change in POLICY_CHANGES {
        let mut f = Fixture::home();
        let now = f.fake.now();
        *f.fake.frozen.lock_or_recover() = Some(now);
        let (mut stream, ended, worker) = f.connection();
        let greeting = joiner::greeting(&mut stream).unwrap();
        change.apply(&mut f);
        attempt_proof(stream, greeting, &f.fake.root);
        assert!(!ended.recv_timeout(HANG).unwrap());
        worker.join().unwrap();
        assert_eq!(f.fake.refusals.load(Ordering::SeqCst), 0, "{change:?}");
        assert!(f.fake.proved.lock_or_recover().is_empty(), "{change:?}");
    }
}

/// The preflight starts on valid policy. Hold it until that policy is invalidated, then let
/// the real client complete possession: the stale connection must never enter the signer.
#[test]
fn an_invalidated_proof_does_not_reach_the_admission_decision() {
    for change in POLICY_CHANGES {
        let mut f = Fixture::home();
        let now = f.fake.now();
        *f.fake.frozen.lock_or_recover() = Some(now);
        let (gate, entered, release) = Gate::pair();
        *f.fake.refusal_gate.lock_or_recover() = Some(gate);
        let (mut stream, ended, worker) = f.connection();
        let greeting = joiner::greeting(&mut stream).unwrap();
        let root = f.fake.root.clone();
        let client = std::thread::spawn(move || attempt_proof(stream, greeting, &root));
        entered.recv_timeout(HANG).unwrap();
        change.apply(&mut f);
        release.send(()).unwrap();
        client.join().unwrap();
        assert!(!ended.recv_timeout(HANG).unwrap());
        worker.join().unwrap();
        assert_eq!(f.fake.refusals.load(Ordering::SeqCst), 1, "{change:?}");
        assert!(f.fake.proved.lock_or_recover().is_empty(), "{change:?}");
    }
}

#[test]
fn a_wrong_signature_and_a_peer_outside_home_never_reach_the_desk() {
    let f = Fixture::home();
    f.ask("forged", true);
    f.ask_from("away", false, "172.20.10.40", false);
    let desk = f.ready_on("home-a");
    assert!(desk.lock_or_recover().rows(f.fake.now()).is_empty());
}

#[test]
fn an_unconfigured_node_refuses_before_the_greeting_and_a_nonhome_node_refuses_asks() {
    let f = Fixture::new(None);
    refused(&f.handle(), "192.168.1.40");
    let f = Fixture::new(Some(Place {
        network: "elsewhere",
        home: false,
    }));
    f.ask_from("away", false, "192.168.1.40", false);
    assert!(
        f.ready_on("elsewhere")
            .lock_or_recover()
            .rows(f.fake.now())
            .is_empty()
    );
}

#[test]
fn membership_arrival_and_policy_replacement_wake_without_a_network_move() {
    let f = Fixture::new(None);
    // The periodic refresh can never become due: only an input-change wake can make progress.
    let now = f.fake.now();
    *f.fake.frozen.lock_or_recover() = Some(now);
    let handle = f.handle();
    refused(&handle, "192.168.1.40");
    *f.fake.place.lock_or_recover() = Some(Place {
        network: "home-a",
        home: true,
    });
    let keys = Arc::new(aterm_net::fleet_member::MemberKeys::default());
    handle.observe_membership(Some(&keys));
    f.ready_on("home-a");
    f.waits.recv_timeout(HANG).unwrap();

    // Ordinary watcher ticks reuse the cached allocation and must not invalidate admission.
    let epoch = handle.shared.state.lock_or_recover().epoch;
    handle.observe_membership(Some(&keys));
    assert_eq!(handle.shared.state.lock_or_recover().epoch, epoch);

    // LocalLog can re-evaluate changed vault inputs to the same keys. That still needs a read.
    let replacement = Arc::new(aterm_net::fleet_member::MemberKeys::default());
    let reads = f.fake.reads.load(Ordering::SeqCst);
    handle.observe_membership(Some(&replacement));
    f.ready_on("home-a");
    assert!(f.fake.reads.load(Ordering::SeqCst) > reads);

    *f.fake.place.lock_or_recover() = None;
    handle.observe_membership(None);
    refused(&handle, "192.168.1.40");
    assert!(handle.shared.state.lock_or_recover().valid_until.is_none());
    assert_eq!(*f.fake.advance.lock_or_recover(), Duration::ZERO);
}

#[test]
fn a_refresh_during_a_read_cannot_publish_its_stale_network() {
    let f = Fixture::home();
    let (gate, entered, release) = Gate::pair();
    *f.fake.read_gate.lock_or_recover() = Some(gate);
    f.handle().refresh();
    entered.recv_timeout(HANG).unwrap();
    *f.fake.place.lock_or_recover() = Some(Place {
        network: "home-b",
        home: true,
    });
    f.handle().refresh();
    refused(&f.handle(), "192.168.1.40");
    let (next_gate, next_entered, next_release) = Gate::pair();
    *f.fake.read_gate.lock_or_recover() = Some(next_gate);
    release.send(()).unwrap();
    next_entered.recv_timeout(HANG).unwrap();
    assert!(
        f.handle()
            .shared
            .state
            .lock_or_recover()
            .valid_until
            .is_none()
    );
    next_release.send(()).unwrap();
    f.ready_on("home-b");
}

/// Dropping the worker refuses new connections at once and joins the worker, which may be in a
/// state read: the drop returns only once that read is let go.
#[test]
fn stopping_refuses_new_connections_and_joins_the_worker() {
    let mut f = Fixture::home();
    let handle = f.handle();
    let (gate, entered, release) = Gate::pair();
    *f.fake.read_gate.lock_or_recover() = Some(gate);
    handle.refresh();
    entered.recv_timeout(HANG).unwrap();
    let service = f.service.take().unwrap();
    let (done, finished) = mpsc::channel();
    let stopping = std::thread::spawn(move || {
        drop(service);
        done.send(()).unwrap();
    });
    let mut state = handle.shared.state.lock_or_recover();
    let end = Instant::now() + HANG;
    while !state.stopped {
        let left = end.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "worker was not stopped");
        state = handle.shared.changed.wait_timeout(state, left).unwrap().0;
    }
    drop(state);
    refused(&handle, "192.168.1.40");
    assert!(
        finished.try_recv().is_err(),
        "the drop returned before its worker's read ended"
    );
    release.send(()).unwrap();
    finished.recv_timeout(HANG).expect("worker joined");
    stopping.join().unwrap();
}
