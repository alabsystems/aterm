// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

// Losing home access is not a change for a connection that began away from home.
const RECOVERY_CHANGES: [PolicyChange; 6] = [
    PolicyChange::Refreshed,
    PolicyChange::Moved,
    PolicyChange::Unavailable,
    PolicyChange::Expired,
    PolicyChange::Stopped,
    PolicyChange::PeriodicMove,
];

fn away() -> Fixture {
    Fixture::new(Some(Place {
        network: "home-a",
        home: false,
    }))
}

#[test]
fn an_invalidated_offhome_recovery_never_starts_the_vault_preflight() {
    for change in RECOVERY_CHANGES {
        let mut f = away();
        let now = f.fake.now();
        *f.fake.frozen.lock_or_recover() = Some(now);
        let (mut stream, ended, worker) = f.connection();
        let greeting = joiner::greeting(&mut stream).unwrap();
        change.apply(&mut f);
        attempt_proof(stream, greeting, &f.fake.root);
        assert!(!ended.recv_timeout(HANG).unwrap(), "{change:?}");
        worker.join().unwrap();
        assert_eq!(f.fake.refusals.load(Ordering::SeqCst), 0, "{change:?}");
        assert!(f.fake.proved.lock_or_recover().is_empty(), "{change:?}");
    }
}

#[test]
fn an_invalidated_offhome_recovery_never_reaches_the_admission_decision() {
    for change in RECOVERY_CHANGES {
        let mut f = away();
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
        assert!(!ended.recv_timeout(HANG).unwrap(), "{change:?}");
        worker.join().unwrap();
        assert_eq!(f.fake.refusals.load(Ordering::SeqCst), 1, "{change:?}");
        assert!(f.fake.proved.lock_or_recover().is_empty(), "{change:?}");
    }
}

type BeforeNow = (std::thread::ThreadId, Box<dyn FnOnce() + Send>);

struct DuringNow {
    inner: Arc<Fake>,
    before: Mutex<Option<BeforeNow>>,
}

impl Backend for DuringNow {
    fn read(&self) -> Result<Option<(Local, String)>> {
        self.inner.read()
    }

    fn now(&self) -> Instant {
        let before = {
            let mut before = self.before.lock_or_recover();
            if before
                .as_ref()
                .is_some_and(|(thread, _)| *thread == std::thread::current().id())
            {
                before.take()
            } else {
                None
            }
        };
        if let Some((_, before)) = before {
            before();
        }
        self.inner.now()
    }

    fn prove(&self, proving: &desk::Proving<'_>, peer: IpAddr) -> Result<SecretBytes> {
        self.inner.prove(proving, peer)
    }

    fn refuses(&self, proving: &desk::Proving<'_>, peer: IpAddr) -> Option<String> {
        self.inner.refuses(proving, peer)
    }

    fn waiting(&self, until: Instant) {
        self.inner.waiting(until);
    }
}

#[test]
fn a_transient_home_read_cannot_promote_an_offhome_proof() {
    let mut f = away();
    drop(f.service.take());
    let backend = Arc::new(DuringNow {
        inner: Arc::clone(&f.fake),
        before: Mutex::new(None),
    });
    f.service = Some(AutomaticJoins::with_backend(backend.clone(), Arc::new(|| {})).unwrap());
    f.waits.recv_timeout(HANG).unwrap();
    let now = f.fake.now();
    *f.fake.frozen.lock_or_recover() = Some(now);
    let desk = f.ready_on("home-a");
    let peer = "192.168.1.40".parse().unwrap();
    assert!(!desk.lock_or_recover().local().automatically_admits(peer));

    let (mut stream, ended, worker) = f.connection();
    let greeting = joiner::greeting(&mut stream).unwrap();
    let away = f.fake.read().unwrap().unwrap().0;
    let home = f.fake.read().unwrap().unwrap().0.with_automatic_lan([]);
    desk.lock_or_recover().refresh(home);
    assert!(desk.lock_or_recover().local().automatically_admits(peer));

    // Only the connection's next clock read can restore away. It is after the desk has
    // captured Proving.lan, before the callback's current-home check; worker clock reads
    // cannot consume it. A proof-classification guard may reject before this read.
    let restore = Arc::clone(&desk);
    let restored = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&restored);
    *backend.before.lock_or_recover() = Some((
        worker.thread().id(),
        Box::new(move || {
            restore.lock_or_recover().refresh(away);
            observed.store(true, Ordering::SeqCst);
        }),
    ));
    attempt_proof(stream, greeting, &f.fake.root);
    let served = ended.recv_timeout(HANG).unwrap();
    worker.join().unwrap();
    backend.before.lock_or_recover().take();
    assert!(!served);
    assert_eq!(
        f.fake.refusals.load(Ordering::SeqCst),
        0,
        "a promoted proof reached preflight (restored={})",
        restored.load(Ordering::SeqCst)
    );
    assert!(f.fake.proved.lock_or_recover().is_empty());
}
