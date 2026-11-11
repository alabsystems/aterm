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
#[path = "watch_tests.rs"]
mod tests;
