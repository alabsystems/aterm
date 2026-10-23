// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Push-stream peer hang-up watcher ([`Peer::Gone`], [`Peer::Unwatched`],
//! [`Watchers`], [`aterm_uds::hangup`], [`PeerGuard`], [`watch_push`]).

use std::io::{Read as _, Write as _};
use std::marker::PhantomData;
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};

use aterm_types::MutexExt as _;
use aterm_uds::CtlStream;

use crate::session_watch::Watchers;

/// What the peer watch knows of one push stream's client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Peer {
    /// Polled, and still there: the thread rings when it goes.
    Watching,
    /// Gone: its stream turned readable (a byte, an end-of-file, an error).
    Gone,
    /// The watch stopped while the client was there: ask the stream itself.
    Unwatched,
}

impl Peer {
    const fn from_u8(raw: u8) -> Peer {
        match raw {
            0 => Peer::Watching,
            1 => Peer::Gone,
            _ => Peer::Unwatched,
        }
    }

    const fn as_u8(self) -> u8 {
        match self {
            Peer::Watching => 0,
            Peer::Gone => 1,
            Peer::Unwatched => 2,
        }
    }
}

/// One watched stream's verdict and wake list.
struct Watched {
    peer: AtomicU8,
    wakes: Arc<Watchers>,
}

impl Watched {
    fn peer(&self) -> Peer {
        Peer::from_u8(self.peer.load(Ordering::Acquire))
    }

    /// Settle a stream still [`Peer::Watching`] at `peer`.
    fn settle(&self, peer: Peer) -> bool {
        self.peer
            .compare_exchange(
                Peer::Watching.as_u8(),
                peer.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// Settle every stream in `watched` at `peer`, THEN ring the lists of those it settled.
fn settle_all(watched: impl IntoIterator<Item = Arc<Watched>>, peer: Peer) -> usize {
    let settled: Vec<Arc<Watched>> = watched.into_iter().filter(|w| w.settle(peer)).collect();
    for w in &settled {
        w.wakes.ring_watchers();
    }
    settled.len()
}

/// What a handle asks of the thread.
enum Command {
    /// Poll `fd` until it turns readable or is released.
    Watch {
        token: u64,
        fd: RawFd,
        watched: Arc<Watched>,
    },
    /// Stop polling `token`'s descriptor, then say so on `released`.
    Release {
        token: u64,
        released: SyncSender<()>,
    },
    /// Stop the thread as a failed poll does.
    #[cfg(test)]
    Fail,
}

/// One polled stream.
struct Entry {
    token: u64,
    fd: RawFd,
    watched: Arc<Watched>,
}

/// The commands posted and not yet taken; `None` once the thread has stopped.
type Inbox = Mutex<Option<Vec<Command>>>;

/// A running peer watch: the handle that posts to its thread.
pub(crate) struct PeerWatch {
    inbox: Arc<Inbox>,
    /// The write end of the thread's bell (non-blocking): a byte wakes its poll.
    bell: UnixStream,
    next_token: AtomicU64,
}

impl PeerWatch {
    /// Start a peer watch's thread; `None` when it cannot start.
    pub(crate) fn start() -> Option<Arc<PeerWatch>> {
        let (bell, listen) = match UnixStream::pair() {
            Ok(pair) => pair,
            Err(error) => {
                aterm_log::warn!("peer watch has no bell: {error}");
                return None;
            }
        };
        if bell.set_nonblocking(true).is_err() || listen.set_nonblocking(true).is_err() {
            aterm_log::warn!("peer watch bell cannot be made non-blocking");
            return None;
        }
        let inbox: Arc<Inbox> = Arc::new(Mutex::new(Some(Vec::new())));
        let polled = Arc::clone(&inbox);
        match std::thread::Builder::new()
            .name("aterm-peer-watch".to_string())
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Responsive);
                watch_thread(&polled, &listen);
            }) {
            Ok(_) => Some(Arc::new(PeerWatch {
                inbox,
                bell,
                next_token: AtomicU64::new(0),
            })),
            Err(error) => {
                aterm_log::warn!("peer watch could not start: {error}");
                None
            }
        }
    }

    /// Watch `stream`'s client until the guard drops; `None` once the thread has stopped.
    pub(crate) fn watch<'a>(self: &Arc<Self>, stream: &'a CtlStream) -> Option<PeerGuard<'a>> {
        let watched = Arc::new(Watched {
            peer: AtomicU8::new(Peer::Watching.as_u8()),
            wakes: Arc::default(),
        });
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let posted = self.post(Command::Watch {
            token,
            fd: stream.as_raw_fd(),
            watched: Arc::clone(&watched),
        });
        posted.then(|| PeerGuard {
            watch: Arc::clone(self),
            token,
            watched,
            _stream: PhantomData,
        })
    }

    /// Post `command` and ring the bell: `false`, the command dropped, once the thread has stopped.
    fn post(&self, command: Command) -> bool {
        {
            let mut inbox = self.inbox.lock_or_recover();
            let Some(pending) = inbox.as_mut() else {
                return false;
            };
            pending.push(command);
        }
        let _ = (&self.bell).write(&[1]);
        true
    }

    /// Stop the thread the way a failed poll does.
    #[cfg(test)]
    pub(crate) fn fail_for_test(&self) {
        let _ = self.post(Command::Fail);
    }
}

/// Watch `stream`'s client on the process's peer watch, started by the first push stream.
pub(crate) fn watch_push(stream: &CtlStream) -> Option<PeerGuard<'_>> {
    static WATCH: OnceLock<Option<Arc<PeerWatch>>> = OnceLock::new();
    WATCH.get_or_init(PeerWatch::start).as_ref()?.watch(stream)
}

/// One push stream on a peer watch.
pub(crate) struct PeerGuard<'a> {
    watch: Arc<PeerWatch>,
    token: u64,
    watched: Arc<Watched>,
    _stream: PhantomData<&'a CtlStream>,
}

impl PeerGuard<'_> {
    /// What the watch knows of the client now.
    pub(crate) fn peer(&self) -> Peer {
        self.watched.peer()
    }

    /// The list rung when the verdict settles.
    pub(crate) fn wakes(&self) -> &Arc<Watchers> {
        &self.watched.wakes
    }
}

impl Drop for PeerGuard<'_> {
    /// Take the stream off the poll set and wait until the thread has let go of its descriptor.
    fn drop(&mut self) {
        let (released, done) = sync_channel(1);
        if self.watch.post(Command::Release {
            token: self.token,
            released,
        }) {
            let _ = done.recv();
        }
    }
}

/// The thread: its loop, then the stop ([`Peer::Unwatched`]).
fn watch_thread(inbox: &Inbox, listen: &UnixStream) {
    let mut entries: Vec<Entry> = Vec::new();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        poll_loop(inbox, listen, &mut entries)
    }))
    .unwrap_or_else(|_| Some("its loop panicked".to_string()));
    let pending = inbox.lock_or_recover().take().unwrap_or_default();
    let posted = pending.into_iter().filter_map(|command| match command {
        Command::Watch { watched, .. } => Some(watched),
        _ => None,
    });
    let handed_back = settle_all(
        entries.into_iter().map(|entry| entry.watched).chain(posted),
        Peer::Unwatched,
    );
    if let Some(why) = failure {
        aterm_log::error!(
            "peer watch STOPPED ({why}): {handed_back} push streams, and every later one until \
             aterm restarts, probe their clients on a timer"
        );
    }
}

/// Poll the bell and watched streams until the bell closes (`None`) or polling fails (`Some`).
fn poll_loop(inbox: &Inbox, listen: &UnixStream, entries: &mut Vec<Entry>) -> Option<String> {
    let mut fds: Vec<libc::pollfd> = Vec::new();
    loop {
        fds.clear();
        fds.push(libc::pollfd {
            fd: listen.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        fds.extend(entries.iter().map(|entry| libc::pollfd {
            fd: entry.fd,
            events: libc::POLLIN,
            revents: 0,
        }));
        let Ok(count) = libc::nfds_t::try_from(fds.len()) else {
            return Some(format!("{} descriptors are too many to poll", fds.len()));
        };
        // SAFETY: `fds` is an initialized, exclusively borrowed array of `count` pollfds.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), count, -1) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Some(format!("its poll failed: {error}"));
        }
        let mut gone = Vec::new();
        for index in (0..entries.len()).rev() {
            if fds[index + 1].revents != 0 {
                gone.push(entries.swap_remove(index).watched);
            }
        }
        settle_all(gone, Peer::Gone);
        if fds[0].revents == 0 {
            continue;
        }
        let mut drain = [0_u8; 64];
        let ended = loop {
            match (&*listen).read(&mut drain) {
                Ok(0) => break true,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break false,
                Err(error) => return Some(format!("its bell failed: {error}")),
            }
        };
        let commands = inbox
            .lock_or_recover()
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        for command in commands {
            match command {
                Command::Watch { token, fd, watched } => {
                    entries.push(Entry { token, fd, watched });
                }
                Command::Release { token, released } => {
                    entries.retain(|entry| entry.token != token);
                    let _ = released.send(());
                }
                #[cfg(test)]
                Command::Fail => return Some("a test stopped it".to_string()),
            }
        }
        if ended {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aterm_spec::wait::MUST_HAPPEN;

    use super::*;
    use crate::session_watch::Subscription;

    fn started() -> Arc<PeerWatch> {
        PeerWatch::start().expect("the peer watch thread starts")
    }

    /// Whether `sub` has a wake pending, without waiting.
    fn rang(sub: &Subscription) -> bool {
        sub.wait(Duration::ZERO)
    }

    /// A QUIET OPEN CLIENT IS KEPT; ONE THAT GOES IS A WAKE. The client's
    /// hang-up, its half-close and a stray byte each settle the stream
    /// `Gone` and ring the list a push loop parks on, with no timer: the
    /// wait is a hang detector, and the ring is the only thing that can end
    /// it. Negative control: the quiet open client is still `Watching`, with
    /// no ring, after the others' polls have run past it.
    #[test]
    fn the_peer_watch_wakes_a_quiet_subscription_on_hangup() {
        let watch = started();
        let (quiet_server, _quiet_client) = CtlStream::pair().expect("pair");
        let quiet = watch.watch(&quiet_server).expect("watched");
        let quiet_sub = Subscription::single(quiet.wakes());
        type End = (&'static str, fn(&CtlStream));
        let ends: [End; 3] = [
            ("hang-up", |client| {
                aterm_uds::hangup::hang_up(client);
            }),
            ("half-close", |client| {
                client
                    .shutdown(std::net::Shutdown::Write)
                    .expect("half-close");
            }),
            ("a stray byte", |client| {
                (&*client).write_all(b"x").expect("byte");
            }),
        ];
        for (how, end) in ends {
            let (server, client) = CtlStream::pair().expect("pair");
            let guard = watch.watch(&server).expect("watched");
            let sub = Subscription::single(guard.wakes());
            assert_eq!(guard.peer(), Peer::Watching, "{how}: there before it");
            end(&client);
            assert!(sub.wait(MUST_HAPPEN), "{how}: the watch rang its list");
            assert_eq!(guard.peer(), Peer::Gone, "{how}");
        }
        assert_eq!(quiet.peer(), Peer::Watching, "a quiet open client stays");
        assert!(!rang(&quiet_sub), "and its list never rang");
    }

    /// A RELEASED DESCRIPTOR IS NEVER POLLED: the guard's drop returns only
    /// once the thread has taken the entry out, so a stream that closes (and
    /// a descriptor number reused) after it is a stranger's. Here the
    /// released stream turns readable and the poll runs past it: a barrier
    /// stream's hang-up must be seen, by a poll that reports every ready
    /// descriptor it holds and stores all its verdicts before any ring.
    /// Negative control: a stream still watched, made readable the same way,
    /// is settled no later than that poll. Counterfactual, run: a release
    /// that leaves the entry in settles the released stream too.
    #[test]
    fn peer_watch_never_polls_a_released_descriptor() {
        let watch = started();
        let (released_server, mut released_client) = CtlStream::pair().expect("pair");
        let released = watch.watch(&released_server).expect("watched");
        let released_verdict = Arc::clone(&released.watched);
        drop(released);
        let (kept_server, mut kept_client) = CtlStream::pair().expect("pair");
        let kept = watch.watch(&kept_server).expect("watched");

        released_client.write_all(b"x").expect("byte");
        kept_client.write_all(b"x").expect("byte");
        let (barrier_server, barrier_client) = CtlStream::pair().expect("pair");
        let barrier = watch.watch(&barrier_server).expect("watched");
        let sub = Subscription::single(barrier.wakes());
        crate::test_socket::hang_up(barrier_client);
        assert!(sub.wait(MUST_HAPPEN), "the barrier's hang-up rang");

        assert_eq!(
            kept.peer(),
            Peer::Gone,
            "a watched readable stream is settled"
        );
        assert_eq!(
            released_verdict.peer(),
            Peer::Watching,
            "a released stream is not polled, readable or not"
        );
    }

    /// A STOPPED WATCH HANDS ITS STREAMS BACK: each reads `Unwatched` and is
    /// rung, so its push loop wakes and probes on a timer again, and a later
    /// watch answers `None`. A client that went before the stop stays
    /// `Gone`. Negative control: before the stop the open client reads
    /// `Watching` with no ring.
    #[test]
    fn a_stopped_peer_watch_hands_its_streams_back() {
        let watch = started();
        let (open_server, _open_client) = CtlStream::pair().expect("pair");
        let open = watch.watch(&open_server).expect("watched");
        let open_sub = Subscription::single(open.wakes());
        let (gone_server, gone_client) = CtlStream::pair().expect("pair");
        let gone = watch.watch(&gone_server).expect("watched");
        let gone_sub = Subscription::single(gone.wakes());
        crate::test_socket::hang_up(gone_client);
        assert!(gone_sub.wait(MUST_HAPPEN), "the hang-up rang");
        assert_eq!(open.peer(), Peer::Watching);
        assert!(!rang(&open_sub), "nothing rang the open client's list yet");

        watch.fail_for_test();
        assert!(
            open_sub.wait(MUST_HAPPEN),
            "the stop rang the open client's list"
        );
        assert_eq!(open.peer(), Peer::Unwatched, "handed back to the probe");
        assert_eq!(gone.peer(), Peer::Gone, "a settled verdict stays");
        let (late_server, _late_client) = CtlStream::pair().expect("pair");
        assert!(
            watch.watch(&late_server).is_none(),
            "a stopped watch refuses"
        );
        drop((open, gone));
    }
}
