// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What wakes a held long-poll with no timer ([`super::park_until_news`]): its client going away,
//! heard by one `vault-held` thread that polls a second descriptor of every held stream, and a
//! change of its vault's store files, heard by a file watch that thread keeps. A long-poll parks
//! on its vault's condition until one of them, news or its hold's end wakes it.
//!
//! Where neither can be heard for a long-poll (the thread, its doorbell, its watch or the
//! stream's second descriptor could not be made, or the thread's poll failed and it stopped),
//! that long-poll looks at its client and its store files every [`UNWATCHED_EVERY`] instead, as
//! every held long-poll did before.

use super::Shared;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// How often a held long-poll looks at its client and its vault's store files where nothing
/// watches them for it ([`Watched::look_every`]).
pub(super) const UNWATCHED_EVERY: Duration = Duration::from_secs(5);

/// The held long-polls the `vault-held` thread watches for ([`start`]).
pub(super) struct Holds {
    set: Mutex<HoldSet>,
    /// Rung when the set changes or the file watch told a change, so the thread looks again.
    bell: Option<aterm_uds::ready::Doorbell>,
    /// Held by a test to hold the thread up each time its poll returns.
    #[cfg(test)]
    pub(super) held_up: Mutex<()>,
}

#[derive(Default)]
struct HoldSet {
    holds: Vec<Hold>,
    next: u64,
    /// Whether the thread runs and polls held clients.
    clients: bool,
    /// Whether its file watch runs.
    files: bool,
    /// The store-file keys the watch told since the thread last looked.
    told: Vec<u64>,
    /// Each vault whose store files the watch watches, by its first key: the envelope file is
    /// that key, the records file the next.
    watched: Vec<(String, u64)>,
    next_key: u64,
}

/// One held long-poll.
struct Hold {
    id: u64,
    /// A second descriptor of the client's stream, polled by the thread until it shows data, an
    /// end or an error; `None` after that, or where no second descriptor could be made.
    stream: Option<Arc<TcpStream>>,
    /// Set by the thread once the stream showed data, an end or an error.
    stirred: Arc<AtomicBool>,
    vault: String,
    /// The condition the long-poll parks on.
    wake: Arc<Condvar>,
}

impl Holds {
    /// No hold yet; nothing watches until [`start`] runs.
    pub(super) fn new() -> Holds {
        Holds {
            set: Mutex::new(HoldSet::default()),
            bell: aterm_uds::ready::Doorbell::new().ok(),
            #[cfg(test)]
            held_up: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HoldSet> {
        self.set.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn ring(&self) {
        if let Some(bell) = &self.bell {
            bell.ring();
        }
    }

    /// Watch `stream`'s client and `vault`'s store files for the long-poll parked on `wake`,
    /// until the [`Watched`] is dropped. Asked under `server_state`, before the long-poll's
    /// first look, so nothing that changes after that look goes unheard.
    pub(super) fn watch(
        &self,
        stream: &TcpStream,
        vault: &str,
        wake: &Arc<Condvar>,
    ) -> Watched<'_> {
        let mut set = self.lock();
        let stream = set
            .clients
            .then(|| stream.try_clone().ok().map(Arc::new))
            .flatten();
        let stirred = Arc::new(AtomicBool::new(false));
        set.next += 1;
        let id = set.next;
        let client = stream.is_some();
        set.holds.push(Hold {
            id,
            stream,
            stirred: Arc::clone(&stirred),
            vault: vault.to_owned(),
            wake: Arc::clone(wake),
        });
        drop(set);
        self.ring();
        Watched {
            holds: self,
            id,
            stirred,
            client,
        }
    }

    /// A file watch told that the store file of `key` changed.
    fn told(&self, key: u64) {
        self.lock().told.push(key);
        self.ring();
    }

    /// How many held clients' streams the thread polls now.
    #[cfg(test)]
    pub(super) fn polled(&self) -> usize {
        self.lock()
            .holds
            .iter()
            .filter(|hold| hold.stream.is_some())
            .count()
    }
}

/// A held long-poll's watch ([`Holds::watch`]), ended when dropped.
pub(super) struct Watched<'h> {
    holds: &'h Holds,
    id: u64,
    stirred: Arc<AtomicBool>,
    /// Whether the thread was given the client's stream to poll.
    client: bool,
}

impl Watched<'_> {
    /// Whether the client's stream showed data, an end or an error: the long-poll looks whether
    /// its client went away ([`super::gone`]).
    pub(super) fn stirred(&self) -> bool {
        self.stirred.load(Ordering::Acquire)
    }

    /// How often the long-poll looks for itself: [`UNWATCHED_EVERY`] while the thread does not
    /// poll its client or no file watch watches its store files, else never.
    pub(super) fn look_every(&self) -> Option<Duration> {
        let set = self.holds.lock();
        (!(self.client && set.clients && set.files)).then_some(UNWATCHED_EVERY)
    }
}

impl Drop for Watched<'_> {
    /// Take the hold out of the set and, where the thread polls the stream's second descriptor,
    /// ring it, so its poll in flight returns and closes that descriptor at once, never waiting
    /// for it to: the long-poll drops this under `server_state` as it answers, and a wait for the
    /// thread's round trip there held every other request up for it, once per long-poll a wake
    /// let go. A socket ends only when its last descriptor closes, so a long-poll that closes its
    /// connection ends it as soon as that poll has returned. (A poll holds each descriptor it asks
    /// after until it returns, so none it names is closed or given to another file under it.)
    fn drop(&mut self) {
        let mut set = self.holds.lock();
        let Some(at) = set.holds.iter().position(|hold| hold.id == self.id) else {
            return;
        };
        let hold = set.holds.swap_remove(at);
        drop(set);
        if hold.stream.is_some() {
            self.holds.ring();
        }
    }
}

/// Start the `vault-held` thread for `shared`'s holds ([`Holds`]): it polls a second descriptor
/// of each held stream, and keeps a file watch on each held vault's store files, until
/// `cancelled` reads its end (the server's [`super::Running`] is stopped or dropped).
///
/// Asked before the server takes its first connection, so every long-poll is watched from its
/// start.
pub(super) fn start(shared: &Arc<Shared>, cancelled: std::io::PipeReader) -> std::io::Result<()> {
    if shared.holds.bell.is_none() {
        return Err(std::io::Error::other("no doorbell for the held long-polls"));
    }
    let files = {
        let shared = Arc::clone(shared);
        aterm_uds::file_watch::FileWatch::platform(
            "vault-held-files",
            Box::new(move |key| shared.holds.told(key)),
        )
        .ok()
    };
    {
        let mut set = shared.holds.lock();
        set.clients = true;
        set.files = files.is_some();
    }
    let running = Arc::clone(shared);
    let started = std::thread::Builder::new()
        .name("vault-held".to_owned())
        .spawn(move || run(&running, &cancelled, files));
    if let Err(error) = started {
        let mut set = shared.holds.lock();
        set.clients = false;
        set.files = false;
        return Err(error);
    }
    Ok(())
}

/// The `vault-held` thread ([`start`]), keeping `files`, the watch of the held vaults' store
/// files where one started.
fn run(
    shared: &Arc<Shared>,
    cancelled: &std::io::PipeReader,
    files: Option<aterm_uds::file_watch::FileWatch>,
) {
    use std::os::fd::AsFd as _;
    let holds = &shared.holds;
    let Some(bell) = &holds.bell else {
        return;
    };
    loop {
        let polled: Vec<(u64, Arc<TcpStream>)> = {
            let mut set = holds.lock();
            if let Some(files) = &files {
                set.watch_files(files, shared);
            }
            set.holds
                .iter()
                .filter_map(|hold| hold.stream.clone().map(|stream| (hold.id, stream)))
                .collect()
        };
        let ready = {
            let mut fds = vec![cancelled.as_fd(), bell.fd()];
            fds.extend(polled.iter().map(|(_, stream)| stream.as_fd()));
            aterm_uds::ready::any(&fds, None)
        };
        let (ended, stirred): (bool, Vec<u64>) = match &ready {
            Ok(ready) => (
                ready.contains(&0),
                ready
                    .iter()
                    .filter_map(|at| at.checked_sub(2).and_then(|at| polled.get(at)))
                    .map(|(id, _)| *id)
                    .collect(),
            ),
            Err(_) => (true, Vec::new()),
        };
        #[cfg(test)]
        drop(holds.held_up.lock().unwrap_or_else(PoisonError::into_inner));
        // The second descriptors of the holds that ended during the poll close here, the last of
        // their sockets' descriptors where the long-poll closed its own ([`Watched::drop`]).
        drop(polled);
        let wakes = {
            let mut set = holds.lock();
            bell.clear();
            let mut wakes: Vec<Arc<Condvar>> = Vec::new();
            for hold in set.holds.iter_mut() {
                if stirred.contains(&hold.id) {
                    // Polled no more: a stream that holds data reads ready until its reader
                    // takes it, which a held long-poll never does.
                    hold.stream = None;
                    hold.stirred.store(true, Ordering::Release);
                    wakes.push(Arc::clone(&hold.wake));
                }
            }
            for key in std::mem::take(&mut set.told) {
                let vault = set
                    .watched
                    .iter()
                    .find(|(_, first)| key == *first || key == *first + 1)
                    .map(|(vault, _)| vault.clone());
                for hold in set
                    .holds
                    .iter()
                    .filter(|hold| vault.as_deref() == Some(hold.vault.as_str()))
                {
                    wakes.push(Arc::clone(&hold.wake));
                }
            }
            if ended {
                // What parks from now on looks for itself.
                set.clients = false;
                set.files = false;
                for hold in &mut set.holds {
                    hold.stream = None;
                }
            }
            wakes
        };
        if ended {
            drop(files);
            wake_all(shared);
            return;
        }
        if !wakes.is_empty() {
            // Under `server_state`, which a long-poll holds from its look until it parks, so a
            // wake never falls between its look and its wait.
            let _state = shared.lock_server_state();
            let mut rung: Vec<&Arc<Condvar>> = Vec::new();
            for wake in &wakes {
                if !rung.iter().any(|done| Arc::ptr_eq(done, wake)) {
                    wake.notify_all();
                    rung.push(wake);
                }
            }
        }
    }
}

/// Wake every held long-poll, under `server_state`, so each looks again and parks as it is
/// watched now.
fn wake_all(shared: &Shared) {
    let wakes: Vec<Arc<Condvar>> = shared
        .holds
        .lock()
        .holds
        .iter()
        .map(|hold| Arc::clone(&hold.wake))
        .collect();
    let _state = shared.lock_server_state();
    for wake in wakes {
        wake.notify_all();
    }
}

impl HoldSet {
    /// Watch the store files of each vault a long-poll is held on, and stop watching those of
    /// vaults none is held on now. A file's first watch tells at once, so a change between a
    /// long-poll's first look and the watch arming wakes it too.
    fn watch_files(&mut self, files: &aterm_uds::file_watch::FileWatch, shared: &Shared) {
        let holds = &self.holds;
        self.watched.retain(|(vault, first)| {
            let held = holds.iter().any(|hold| hold.vault == *vault);
            if !held {
                files.unwatch(*first);
                files.unwatch(*first + 1);
            }
            held
        });
        for hold in &self.holds {
            if self.watched.iter().any(|(vault, _)| *vault == hold.vault) {
                continue;
            }
            let first = self.next_key;
            self.next_key += 2;
            files.watch(first, shared.vault_path(&hold.vault));
            files.watch(first + 1, shared.records_path(&hold.vault));
            self.watched.push((hold.vault.clone(), first));
        }
    }
}
