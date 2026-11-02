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
    place: Mutex<Option<Place>>,
    read_gate: Mutex<Option<Gate>>,
    post_gate: Mutex<Option<Gate>>,
    fail_read: AtomicBool,
    fail_post: AtomicBool,
    posts: Sender<String>,
    waiting: Sender<Instant>,
    posted: AtomicUsize,
    reads: AtomicUsize,
    post_reads: Mutex<Vec<usize>>,
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

    fn forward(
        &self,
        desk: &Mutex<Desk>,
        now: Instant,
        current: &dyn Fn() -> bool,
    ) -> Result<usize> {
        desk::forward_automatic_with(desk, now, |request| {
            if !current() {
                return Ok(());
            }
            self.posted.fetch_add(1, Ordering::SeqCst);
            self.post_reads
                .lock_or_recover()
                .push(self.reads.load(Ordering::SeqCst));
            self.posts.send(request.host.clone()).unwrap();
            if let Some(gate) = self.post_gate.lock_or_recover().take() {
                gate.wait();
            }
            if self.fail_post.swap(false, Ordering::SeqCst) {
                Err(Error::new("vault unreachable"))
            } else {
                Ok(())
            }
        })
    }

    fn now(&self) -> Instant {
        Instant::now() + *self.advance.lock_or_recover()
    }

    fn waiting(&self, until: Instant) {
        self.waiting.send(until).unwrap();
    }
}

struct Fixture {
    service: Option<AutomaticJoins>,
    fake: Arc<Fake>,
    posts: Receiver<String>,
    waits: Receiver<Instant>,
    _root: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new(place: Option<Place>) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let (posts, posted) = mpsc::channel();
        let (waiting, waits) = mpsc::channel();
        let fake = Arc::new(Fake {
            root: root.path().to_owned(),
            advance: Mutex::new(Duration::ZERO),
            place: Mutex::new(place),
            read_gate: Mutex::new(None),
            post_gate: Mutex::new(None),
            fail_read: AtomicBool::new(false),
            fail_post: AtomicBool::new(false),
            posts,
            waiting,
            posted: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
            post_reads: Mutex::new(Vec::new()),
        });
        let service = AutomaticJoins::with_backend(fake.clone()).unwrap();
        // This observes completion of the initial read and empty post, without a sleep.
        waits.recv_timeout(HANG).unwrap();
        Self {
            service: Some(service),
            fake,
            posts: posted,
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
        let (mut stream, ended, worker) = self.connection();
        let mut greeting = joiner::greeting(&mut stream).unwrap();
        if wrong_member {
            greeting.key[0] ^= 1;
        }
        let device = device(&self.fake.root, name);
        let joining = Joining {
            device: &device,
            node: [7; 32],
            name: name.to_owned(),
            model: "Mac".to_owned(),
        };
        assert_eq!(
            joiner::ask(&mut stream, &greeting, &joining).is_ok(),
            !wrong_member
        );
        assert_eq!(
            ended.recv_timeout(HANG).expect("ask handler finished"),
            !wrong_member
        );
        worker.join().unwrap();
    }

    fn connection(&self) -> (TcpStream, Receiver<bool>, JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = self.handle();
        let (ended, end) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(HANG)).unwrap();
            stream.set_write_timeout(Some(HANG)).unwrap();
            let result = handle.serve(&mut stream, "192.168.1.40".parse().unwrap());
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

/// Both connection threads finish while the sole submission worker is deliberately blocked.
/// No latency budget is involved: the vault cannot progress until this test releases it.
#[test]
fn a_slow_vault_does_not_hold_an_ask_connection_or_spawn_another_worker() {
    let f = Fixture::home();
    let (gate, entered, release) = Gate::pair();
    *f.fake.post_gate.lock_or_recover() = Some(gate);
    f.ask("first", false);
    entered.recv_timeout(HANG).unwrap();
    f.ask("second", false);
    assert_eq!(f.fake.posted.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    assert_eq!(f.posts.recv_timeout(HANG).unwrap(), "first");
    assert_eq!(f.posts.recv_timeout(HANG).unwrap(), "second");
}

#[test]
fn each_waiting_machine_gets_a_fresh_policy_read_before_its_submission() {
    let f = Fixture::home();
    let (gate, entered, release) = Gate::pair();
    *f.fake.read_gate.lock_or_recover() = Some(gate);
    f.ask("first", false);
    entered.recv_timeout(HANG).unwrap();
    // Both asks are already queued when the worker's policy read finishes.
    f.ask("second", false);
    release.send(()).unwrap();
    assert_eq!(f.posts.recv_timeout(HANG).unwrap(), "first");
    assert_eq!(f.posts.recv_timeout(HANG).unwrap(), "second");
    let reads = f.fake.post_reads.lock_or_recover();
    assert_eq!(reads.len(), 2);
    assert!(
        reads[1] > reads[0],
        "one policy read covered both posts: {reads:?}"
    );
}

#[test]
fn a_failed_post_retries_at_its_deadline_without_another_ask() {
    let f = Fixture::home();
    f.fake.fail_post.store(true, Ordering::SeqCst);
    f.ask("retry", false);
    assert_eq!(f.posts.recv_timeout(HANG).unwrap(), "retry");
    f.waits.recv_timeout(HANG).unwrap();
    assert_eq!(f.fake.posted.load(Ordering::SeqCst), 1);
    *f.fake.advance.lock_or_recover() += Duration::from_secs(30);
    // Wake the condvar as if its timer expired; no ask or refresh revision changes.
    f.handle().shared.changed.notify_all();
    assert_eq!(f.posts.recv_timeout(HANG).unwrap(), "retry");
    assert_eq!(f.fake.posted.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failed_membership_read_retries_only_requests_that_are_still_live() {
    for (elapsed, retry) in [(REFRESH_EVERY, true), (desk::STILL_ASKING, false)] {
        let f = Fixture::home();
        f.fake.fail_post.store(true, Ordering::SeqCst);
        f.ask("retry-after-read", false);
        assert_eq!(f.posts.recv_timeout(HANG).unwrap(), "retry-after-read");
        f.waits.recv_timeout(HANG).unwrap();

        let desk = f.ready_on("home-a");
        f.fake.fail_read.store(true, Ordering::SeqCst);
        f.handle().refresh();
        f.waits.recv_timeout(HANG).unwrap();
        refused(&f.handle(), "192.168.1.40");
        assert_eq!(desk.lock_or_recover().rows(f.fake.now()).len(), 1);
        assert_eq!(f.fake.posted.load(Ordering::SeqCst), 1);

        *f.fake.advance.lock_or_recover() += elapsed;
        f.handle().shared.changed.notify_all();
        f.waits.recv_timeout(HANG).unwrap();
        assert!(Arc::ptr_eq(&desk, &f.ready_on("home-a")));
        if retry {
            assert_eq!(f.posts.recv_timeout(HANG).unwrap(), "retry-after-read");
            assert_eq!(f.fake.posted.load(Ordering::SeqCst), 2);
        } else {
            assert!(f.posts.try_recv().is_err(), "expired request was posted");
            assert_eq!(f.fake.posted.load(Ordering::SeqCst), 1);
            let desk = desk.lock_or_recover();
            assert!(desk.rows(f.fake.now()).is_empty());
            assert!(desk.next_automatic_request(f.fake.now()).is_none());
        }
    }
}

#[test]
fn a_wrong_signature_and_a_peer_outside_home_never_reach_the_queue() {
    let f = Fixture::home();
    f.ask("forged", true);
    refused(&f.handle(), "172.20.10.40");
    let desk = f.ready_on("home-a");
    assert!(desk.lock_or_recover().rows(f.fake.now()).is_empty());
    assert_eq!(f.fake.posted.load(Ordering::SeqCst), 0);
}

#[test]
fn an_unconfigured_or_nonhome_node_refuses_before_the_greeting() {
    for place in [
        None,
        Some(Place {
            network: "elsewhere",
            home: false,
        }),
    ] {
        let f = Fixture::new(place);
        refused(&f.handle(), "192.168.1.40");
        assert_eq!(f.fake.posted.load(Ordering::SeqCst), 0);
    }
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
    assert_eq!(f.fake.posted.load(Ordering::SeqCst), 0);
}

#[test]
fn a_home_policy_change_during_a_post_prevents_the_waiting_request() {
    let f = Fixture::home();
    let (gate, entered, release) = Gate::pair();
    *f.fake.post_gate.lock_or_recover() = Some(gate);
    f.ask("first", false);
    entered.recv_timeout(HANG).unwrap();
    f.ask("waiting", false);
    *f.fake.place.lock_or_recover() = Some(Place {
        network: "home-a",
        home: false,
    });
    f.handle().refresh();
    release.send(()).unwrap();
    f.waits.recv_timeout(HANG).unwrap();
    refused(&f.handle(), "192.168.1.40");
    assert_eq!(f.fake.posted.load(Ordering::SeqCst), 1);
}

#[test]
fn a_connection_that_read_the_old_network_cannot_queue_on_the_new_one() {
    let f = Fixture::home();
    let old = f.ready_on("home-a");
    let (mut stream, ended, worker) = f.connection();
    let greeting = joiner::greeting(&mut stream).unwrap();
    *f.fake.place.lock_or_recover() = Some(Place {
        network: "home-b",
        home: true,
    });
    f.handle().refresh();
    let new = f.ready_on("home-b");
    assert!(!Arc::ptr_eq(&old, &new));
    let device = device(&f.fake.root, "moved");
    let joining = Joining {
        device: &device,
        node: [7; 32],
        name: "moved".to_owned(),
        model: "Mac".to_owned(),
    };
    joiner::ask(&mut stream, &greeting, &joining).unwrap();
    assert!(ended.recv_timeout(HANG).unwrap());
    worker.join().unwrap();
    assert_eq!(old.lock_or_recover().rows(f.fake.now()).len(), 1);
    assert!(new.lock_or_recover().rows(f.fake.now()).is_empty());
    assert_eq!(f.fake.posted.load(Ordering::SeqCst), 0);
}

#[test]
fn stopping_refuses_new_connections_and_joins_the_in_flight_submission() {
    let mut f = Fixture::home();
    let handle = f.handle();
    let (gate, entered, release) = Gate::pair();
    *f.fake.post_gate.lock_or_recover() = Some(gate);
    f.ask("last", false);
    entered.recv_timeout(HANG).unwrap();
    f.ask("waiting", false);
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
    release.send(()).unwrap();
    finished.recv_timeout(HANG).expect("worker joined");
    stopping.join().unwrap();
    assert_eq!(f.fake.posted.load(Ordering::SeqCst), 1);
}
