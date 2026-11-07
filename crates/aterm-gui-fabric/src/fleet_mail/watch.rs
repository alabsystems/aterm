// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The worker that reads each session's direct mailbox again ([`super::restore`]) after another
//! process wrote it, a handoff peer included. A parked `await inbox` is woken only by a refresh
//! that changed what the session knows, so nothing else rechecks.
//!
//! It hears a mailbox replaced through a [`FileWatch`]: every write publishes a new file by
//! rename (`write_private`), and the watch hears the file and its folder. The first look a
//! session's ownership allows makes that folder, as a transaction makes it, so the watch listens
//! there and not on the busy state folder above it; until then it listens on the nearest folder
//! above that is there. While every session is read, the worker waits with no deadline, so
//! nothing runs on a clock at rest. A look is owed, and taken again each [`RETRY`]
//! until it finishes, when a writer held the lock, the read failed, or the process is an
//! uncommitted handoff candidate, which does no mailbox I/O until Commit. Where file events do
//! not start, every session is looked at each [`RETRY`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak, mpsc};
use std::time::Duration;

use aterm_session::SessionId;
use aterm_uds::file_watch::FileWatch;

use super::{Stamp, restore, stamp};
use crate::FabricSession;

/// How soon a look that could not finish is taken again.
pub const RETRY: Duration = Duration::from_secs(1);

/// What the worker hears.
enum Tell<S> {
    /// A session whose mailbox to keep read.
    Watch(Weak<S>),
    /// This session closed.
    Forget(SessionId),
    /// The file watch saw this key's mailbox change, or took its first look at it.
    Changed(u64),
    /// The door closed.
    End,
}

/// Where the worker hears each mailbox: a [`FileWatch`], or nothing ([`Deaf`]).
trait Arms {
    fn watch(&self, key: u64, path: PathBuf);
    fn unwatch(&self, key: u64);
}

impl Arms for FileWatch {
    fn watch(&self, key: u64, path: PathBuf) {
        FileWatch::watch(self, key, path);
    }
    fn unwatch(&self, key: u64) {
        FileWatch::unwatch(self, key);
    }
}

/// No file events: every session is looked at each [`RETRY`].
struct Deaf;

impl Arms for Deaf {
    fn watch(&self, _: u64, _: PathBuf) {}
    fn unwatch(&self, _: u64) {}
}

/// One watched session.
struct Entry<S> {
    session: Weak<S>,
    sid: SessionId,
    /// Its mailbox; `None` where it has none, which is never read.
    path: Option<PathBuf>,
    /// The mailbox's stamp at the last look that finished; `None` while there was no mailbox.
    seen: Option<Stamp>,
    /// A look is due.
    owed: bool,
    /// The mailbox's folder is there: made, or found private.
    made: bool,
}

/// The worker's state: each watched session under the key its mailbox is watched by.
struct Sessions<S> {
    entries: BTreeMap<u64, Entry<S>>,
    next: u64,
    /// No file events: every look is owed.
    deaf: bool,
}

impl<S: FabricSession> Sessions<S> {
    const fn new(deaf: bool) -> Self {
        Self {
            entries: BTreeMap::new(),
            next: 0,
            deaf,
        }
    }

    /// Take in `tell`; false when the door closed.
    fn hear(&mut self, tell: Tell<S>, arms: &dyn Arms) -> bool {
        match tell {
            Tell::Watch(weak) => {
                let Some(session) = weak.upgrade() else {
                    return true;
                };
                let key = self.next;
                self.next += 1;
                let path = session.fleet_mail_path();
                if let Some(path) = &path {
                    arms.watch(key, path.clone());
                }
                // The first look is owed: what the session knows was read before this, and an
                // existing mailbox differs from no stamp.
                self.entries.insert(
                    key,
                    Entry {
                        session: weak,
                        sid: session.self_id().clone(),
                        path,
                        seen: None,
                        owed: true,
                        made: false,
                    },
                );
            }
            Tell::Forget(sid) => self.entries.retain(|key, entry| {
                let keep = entry.sid != sid;
                if !keep {
                    arms.unwatch(*key);
                }
                keep
            }),
            Tell::Changed(key) => {
                if let Some(entry) = self.entries.get_mut(&key) {
                    entry.owed = true;
                }
            }
            Tell::End => return false,
        }
        true
    }

    /// Look at each owed session: restore one whose mailbox's stamp moved since its last look
    /// that finished, and keep it owed while that cannot finish. A session that is gone is
    /// unwatched.
    fn look(&mut self, arms: &dyn Arms) {
        let deaf = self.deaf;
        self.entries.retain(|key, entry| {
            let Some(session) = entry.session.upgrade() else {
                arms.unwatch(*key);
                return false;
            };
            if !(deaf || entry.owed) {
                return true;
            }
            if !session.fleet_mail_ready() {
                entry.owed = true;
                return true;
            }
            if !entry.made {
                // One that cannot be made yet leaves the watch above it, and is tried again at
                // the next look.
                entry.made = entry
                    .path
                    .as_deref()
                    .and_then(std::path::Path::parent)
                    .is_none_or(|dir| aterm_uds::private_file::create_private_dir(dir).is_ok());
            }
            let now = entry.path.as_deref().and_then(|path| stamp(path).ok());
            entry.owed = now != entry.seen && !restore(&*session);
            if !entry.owed {
                entry.seen = now;
            }
            true
        });
    }

    /// How long the worker may wait for its next tell: `None`, with no deadline, while no look
    /// is owed.
    fn wait(&self) -> Option<Duration> {
        self.entries
            .values()
            .any(|entry| self.deaf || entry.owed)
            .then_some(RETRY)
    }
}

/// Serve `tells` until the door closes: take in every tell queued, look, then wait.
fn serve<S: FabricSession>(
    tells: &mpsc::Receiver<Tell<S>>,
    arms: &dyn Arms,
    mut sessions: Sessions<S>,
) {
    loop {
        let first = match sessions.wait() {
            None => tells
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            Some(timeout) => tells.recv_timeout(timeout),
        };
        match first {
            Ok(tell) => {
                if !sessions.hear(tell, arms) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        while let Ok(tell) = tells.try_recv() {
            if !sessions.hear(tell, arms) {
                return;
            }
        }
        sessions.look(arms);
    }
}

/// The door to one worker thread, `fleet-mail-watch`. Dropping it ends the worker.
pub struct Door<S> {
    tells: mpsc::Sender<Tell<S>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl<S: FabricSession + 'static> Door<S> {
    /// Start the worker over this platform's file events, on its own thread
    /// `fleet-mail-events`; where they do not start, the worker looks each [`RETRY`].
    ///
    /// # Errors
    /// When the worker's thread cannot start.
    pub fn spawn() -> std::io::Result<Self> {
        Self::spawn_telling(|_| {})
    }

    /// [`Self::spawn`], calling `told` with each key the file watch tells of as it tells it.
    fn spawn_telling(told: impl Fn(u64) + Send + 'static) -> std::io::Result<Self> {
        let (tells, heard) = mpsc::channel();
        let changed = tells.clone();
        let events = FileWatch::platform(
            "fleet-mail-events",
            Box::new(move |key| {
                told(key);
                let _ = changed.send(Tell::Changed(key));
            }),
        );
        let worker = std::thread::Builder::new()
            .name("fleet-mail-watch".to_owned())
            .spawn(move || match events {
                Ok(watch) => serve(&heard, &watch, Sessions::new(false)),
                Err(why) => {
                    aterm_log::info!(
                        "fleet mail: no file events ({why}), so each mailbox is stamped every second"
                    );
                    serve(&heard, &Deaf, Sessions::new(true));
                }
            })?;
        Ok(Self {
            tells,
            worker: Some(worker),
        })
    }

    /// Keep `session`'s mailbox read; false when the worker has ended.
    #[must_use]
    pub fn watch(&self, session: &Arc<S>) -> bool {
        self.tells
            .send(Tell::Watch(Arc::downgrade(session)))
            .is_ok()
    }

    /// Stop reading `sid`'s mailbox: the session closed.
    pub fn forget(&self, sid: &SessionId) {
        let _ = self.tells.send(Tell::Forget(sid.clone()));
    }
}

impl<S> Drop for Door<S> {
    fn drop(&mut self) {
        let _ = self.tells.send(Tell::End);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
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
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the platform's tell",
            || (tells.load(Ordering::SeqCst) >= 2).then_some(()),
        );
        drop(door);
    }
}
