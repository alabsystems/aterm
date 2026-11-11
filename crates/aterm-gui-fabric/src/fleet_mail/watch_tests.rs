// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::fleet_mail::tests::{Session, as_node, session};
use crate::fleet_mail::{STAMPS, TRANSACTIONS, post};
use std::sync::Mutex;
use std::sync::atomic::Ordering;

/// What a test's worker was asked to hear: each watched key and its path.
#[derive(Default)]
struct Recorded(Mutex<BTreeMap<u64, PathBuf>>);

impl Arms for Recorded {
    fn watch(&self, key: u64, path: PathBuf) {
        self.0.lock().unwrap().insert(key, path);
    }
    fn unwatch(&self, key: u64) {
        self.0.lock().unwrap().remove(&key);
    }
}

impl Recorded {
    fn keys(&self) -> Vec<u64> {
        self.0.lock().unwrap().keys().copied().collect()
    }
}

fn transactions() -> u64 {
    TRANSACTIONS.with(std::cell::Cell::get)
}

fn stamps() -> u64 {
    STAMPS.with(std::cell::Cell::get)
}

/// A receiver whose mailbox already holds one message from a peer.
fn written(scratch: &std::path::Path) -> (Arc<Session>, Session) {
    let path = scratch.join("private/inbox.toml");
    let receiver = Arc::new(session(&path));
    let peer = session(&path);
    assert_eq!(
        as_node("a", || post(&peer, "key=one kind=ask -- first")),
        "OK f1\n"
    );
    (receiver, peer)
}

#[test]
fn a_read_mailbox_owes_no_look_and_sets_the_wait_no_deadline() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (receiver, _peer) = written(scratch.path());
    let arms = Recorded::default();
    let mut sessions = Sessions::new(false);
    assert_eq!(sessions.wait(), None, "nothing watched");
    assert!(sessions.hear(Tell::Watch(Arc::downgrade(&receiver)), &arms));
    assert_eq!(
        arms.0.lock().unwrap().get(&0),
        Some(&receiver.path),
        "the mailbox is heard before its first look"
    );
    assert_eq!(sessions.wait(), Some(RETRY), "the first look is owed");
    sessions.look(&arms);
    assert_eq!(receiver.fabric.last_direct_mail_id(), 1);
    assert_eq!(sessions.wait(), None, "read: at rest");

    // The negative control: with no file events every look is owed, so the worker waits on
    // the clock.
    let mut deaf = Sessions::new(true);
    assert!(deaf.hear(Tell::Watch(Arc::downgrade(&receiver)), &Deaf));
    deaf.look(&Deaf);
    assert_eq!(deaf.wait(), Some(RETRY));
}

#[test]
fn a_changed_mailbox_is_read_once_and_an_unchanged_one_not_at_all() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (receiver, peer) = written(scratch.path());
    let arms = Recorded::default();
    let mut sessions = Sessions::new(false);
    assert!(sessions.hear(Tell::Watch(Arc::downgrade(&receiver)), &arms));
    sessions.look(&arms);
    let read = transactions();
    assert!(sessions.hear(Tell::Changed(0), &arms));
    sessions.look(&arms);
    assert_eq!(
        transactions(),
        read,
        "a tell whose stamp did not move reads nothing"
    );
    assert_eq!(sessions.wait(), None);
    assert_eq!(
        as_node("b", || post(&peer, "key=two kind=ask -- second")),
        "OK f2\n"
    );
    // The peer's post ran on this thread too.
    let posted = transactions();
    sessions.look(&arms);
    assert_eq!(
        (transactions(), receiver.fabric.last_direct_mail_id()),
        (posted, 1),
        "a look nothing told of is not taken"
    );
    assert!(sessions.hear(Tell::Changed(0), &arms));
    sessions.look(&arms);
    assert_eq!(transactions(), posted + 1);
    assert_eq!(receiver.fabric.last_direct_mail_id(), 2);
    assert_eq!(sessions.wait(), None);
}

#[test]
fn a_look_that_cannot_finish_is_owed_until_one_does() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (receiver, _peer) = written(scratch.path());
    let arms = Recorded::default();
    let mut sessions = Sessions::new(false);
    receiver.ready.store(false, Ordering::SeqCst);
    assert!(sessions.hear(Tell::Watch(Arc::downgrade(&receiver)), &arms));
    let before = (transactions(), stamps());
    sessions.look(&arms);
    assert_eq!(
        (transactions(), stamps()),
        before,
        "an uncommitted candidate does no mailbox I/O"
    );
    assert_eq!(sessions.wait(), Some(RETRY));
    receiver.ready.store(true, Ordering::SeqCst);

    let held = aterm_uds::private_file::PrivateLock::take(
        receiver.path.parent().unwrap(),
        "inbox.toml.lock",
        Duration::ZERO,
        "test lock",
    )
    .unwrap();
    sessions.look(&arms);
    assert_eq!(
        receiver.fabric.last_direct_mail_id(),
        0,
        "a writer holds the lock"
    );
    assert_eq!(sessions.wait(), Some(RETRY));
    drop(held);
    sessions.look(&arms);
    assert_eq!(receiver.fabric.last_direct_mail_id(), 1);
    assert_eq!(sessions.wait(), None);
}

#[test]
fn a_ready_look_makes_the_mailbox_folder_and_a_candidate_makes_none() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let folder = scratch.path().join("private");
    let receiver = Arc::new(session(&folder.join("inbox.toml")));
    let arms = Recorded::default();
    let mut sessions = Sessions::new(false);
    receiver.ready.store(false, Ordering::SeqCst);
    assert!(sessions.hear(Tell::Watch(Arc::downgrade(&receiver)), &arms));
    sessions.look(&arms);
    assert!(!folder.exists(), "an uncommitted candidate makes no folder");
    receiver.ready.store(true, Ordering::SeqCst);
    sessions.look(&arms);
    let made = std::fs::metadata(&folder).expect("the ready look made the folder");
    assert!(made.is_dir());
    assert_eq!(made.permissions().mode() & 0o777, 0o700, "private");
    assert_eq!(sessions.wait(), None, "no mailbox yet: nothing owed");
}

#[test]
fn a_closed_or_dropped_session_is_no_longer_heard() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (receiver, _peer) = written(scratch.path());
    let other = Arc::new(Session {
        sid: SessionId::new("s-87654321"),
        ..session(&scratch.path().join("private/other.toml"))
    });
    let arms = Recorded::default();
    let mut sessions = Sessions::new(false);
    assert!(sessions.hear(Tell::Watch(Arc::downgrade(&receiver)), &arms));
    assert!(sessions.hear(Tell::Watch(Arc::downgrade(&other)), &arms));
    sessions.look(&arms);
    assert_eq!(arms.keys(), [0, 1]);
    assert!(sessions.hear(Tell::Forget(receiver.sid.clone()), &arms));
    assert_eq!(arms.keys(), [1], "closed");
    drop(other);
    sessions.look(&arms);
    assert_eq!(arms.keys(), Vec::<u64>::new(), "dropped");
    assert_eq!(sessions.wait(), None);
    assert!(!sessions.hear(Tell::End, &arms));
}

#[test]
fn a_peers_write_wakes_a_parked_inbox_wait_through_the_platforms_file_events() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (receiver, peer) = written(scratch.path());
    let tells = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let door = Door::spawn_telling({
        let tells = Arc::clone(&tells);
        move |_| {
            tells.fetch_add(1, Ordering::SeqCst);
        }
    })
    .unwrap();
    assert!(door.watch(&receiver));
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the first look", || {
        (receiver.fabric.last_direct_mail_id() == 1).then_some(())
    });
    // The file watch's own first look tells whatever it finds, so the peer writes after it:
    // the tell that follows is the platform's event, which a deaf source never gives.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the watch's first look",
        || (tells.load(Ordering::SeqCst) >= 1).then_some(()),
    );
    assert!(crate::cmd_inbox(&*receiver, "").contains("msg f1 "));
    // Past the hang detector below, so the answer is a ring's, never the wait's deadline (a
    // wait that times out reads the mailbox as it answers); and the worker, at rest, waits
    // with no deadline, so only the file watch's tell can bring that ring.
    let timeout = u64::try_from((2 * aterm_spec::wait::MUST_HAPPEN).as_millis()).unwrap();
    let parks = || receiver.fabric.inbox_parks.load(Ordering::SeqCst);
    let before = parks();
    let answer = std::thread::scope(|scope| {
        /// A failed assertion ends the wait at once, not at its deadline.
        struct Release<'a>(&'a crate::SessionFabric);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    self.0.close();
                }
            }
        }
        let (tx, rx) = mpsc::channel();
        let waiter = &*receiver;
        scope.spawn(move || {
            // Unheard once a failed assertion has gone.
            let _ = tx.send(crate::cmd_await_inbox(waiter, &["since=0"], timeout));
        });
        let _release = Release(&receiver.fabric);
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the wait parks", || {
            (parks() > before).then_some(())
        });
        assert_eq!(
            as_node("b", || post(&peer, "key=two kind=ask -- second")),
            "OK f2\n"
        );
        let answer = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        if answer.is_err() {
            receiver.fabric.wake_waiters_for_test();
        }
        answer
    });
    assert_eq!(
        answer.expect("the parked wait answers"),
        "OK inbox 0 fleet=f2\n"
    );
    // The worker may take the look the first tell owed only after the post, and so read f2
    // without any event; a tell beyond the first look, which the post's rename brings, is the
    // platform's own, and a deaf source never gives one.
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the platform's tell", || {
        (tells.load(Ordering::SeqCst) >= 2).then_some(())
    });
    drop(door);
}
