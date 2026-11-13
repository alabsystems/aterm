// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What of aterm's reaches the LAN through the root LAN broker on macOS
//! (`docs/DESIGN-lan-broker-2026-11-05.md`), or while none is installed through the allowed
//! direct path (`aterm_uds::lan::direct`), and whether either answers: a vault at a URL on the
//! LAN is reached through them, so a step that would reach one waits while neither does; a vault
//! at a path, or at a URL off the LAN, waits for nothing.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_uds::file_watch::{FileStamp, FileWatch};

/// Why aterm cannot reach the LAN on macOS: the LAN broker's hello's error in its own words (not
/// installed and no allowed direct path, another protocol, refused, not checked now), from
/// `aterm_uds::lan::broker::hello`. `None` where a hello is answered (by the broker, or while
/// none is installed by the allowed direct path, `aterm_uds::lan::direct`), and off macOS, where
/// the LAN is reached in-process.
#[must_use]
pub fn broker_unready() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    aterm_uds::lan::broker::hello()
        .err()
        .map(|why| why.to_string())
}

/// Whether `url`'s host is on the LAN, so aterm reaches it through the LAN broker on macOS: a
/// local name (`alab-secrets.local`, a single label) or a local-network address
/// ([`aterm_uds::lan::is_local_name`], [`aterm_uds::lan::is_local_network`]). A source that is no
/// URL (a vault at a path) is not.
#[must_use]
pub fn on_the_lan(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or(bracketed),
        None => authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host),
    };
    match host.parse::<std::net::IpAddr>() {
        Ok(addr) => aterm_uds::lan::is_local_network(&addr),
        Err(_) => aterm_uds::lan::is_local_name(host),
    }
}

/// How often [`watch_arrivals`] asks while none answers where no file events can be had: a
/// connect to a socket that is not there fails at once, so this costs nothing, and a step waiting
/// for the broker goes on within it of the elevation that installs it.
const ABSENT_POLL: Duration = Duration::from_secs(2);

/// How often [`watch_arrivals`] asks while one answers where no file events can be had, to hear
/// it go (a repair, an upgrade that restarts it) and so its next arrival.
const PRESENT_POLL: Duration = Duration::from_secs(30);

/// The wait before a hello that failed in a way nothing heard ends ([`Answer::Transient`]) is
/// asked again, doubling at each such failure in a row up to [`RETRY_MOST`].
const RETRY_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between asks of a hello that keeps failing in a way nothing heard ends.
const RETRY_MOST: Duration = Duration::from_secs(30);

/// The [`FileWatch`] key of the broker socket's folder.
const SOCKET_FOLDER: u64 = 1;

/// The [`FileWatch`] key of the file macOS keeps the direct path's allowance in.
const ALLOWANCE: u64 = 2;

/// Call `arrived` each time a hello is answered ([`broker_unready`]: the LAN broker's, or while
/// none is installed the allowed direct path's) where the ask before it was not, and each time
/// one is answered from elsewhere than the last answer: by a broker whose socket was made again
/// since (a repair, an upgrade or an adopted update restarted it), or by the direct path where the
/// broker answered (or the other way), for as long as this process lives: a step deferred until
/// one answers (the vault's look, a reach of a vault on the LAN) is woken by its arrival, never by
/// a person. What answers at its first ask is no arrival.
///
/// It asks only when what can move the answer changed: the folder of the broker's socket, in which
/// every broker that starts clears and binds its socket again (one that goes leaves its socket
/// there, refusing connections), or the file macOS keeps the direct path's allowance in
/// ([`aterm_uds::lan::direct::record`]), each heard through a [`FileWatch`] on a thread of its own
/// (`lan-broker-events`) whose first look at each comes before the first ask; and again, from
/// [`RETRY_FIRST`] doubling to [`RETRY_MOST`], while a hello fails in a way nothing heard ends
/// ([`Answer::Transient`]). A process the broker refuses by its code asks nothing more until the
/// broker's socket is made again. Where no file events can be had it asks every [`ABSENT_POLL`]
/// while none answers and every [`PRESENT_POLL`] while one does. Off macOS, where nothing waits
/// for a broker, it returns at once.
pub fn watch_arrivals(arrived: impl FnMut()) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let socket = aterm_uds::lan::dialled_socket();
    let mut ears = Watched::start(&socket, aterm_uds::lan::direct::record());
    arrivals(|| ask(&socket), &mut ears, arrived, None);
}

/// How one hello went, as [`watch_arrivals`] waits on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// Answered: by this process's own allowed direct path (`direct`), or by the broker.
    Answered {
        /// The direct path answered.
        direct: bool,
    },
    /// Not answered, in a way only a change the watch hears ends: no socket and no allowance, a
    /// broker of another protocol, or one that refused this process's code or is not the broker
    /// (a broker that starts makes its socket again). A broker that predates
    /// `Unavailable::Unchecked` also refuses while it could not check: it adopts the update that
    /// brings that, its socket made again.
    Settled,
    /// Not answered, in a way that can end with nothing heard: a socket there that nothing listens
    /// on (a broker between its bind and its listen, or one that went and left it), a broker that
    /// did not judge this process (its code check did not finish, or it was serving its most
    /// connections), or an exchange that failed or hung.
    Transient,
}

impl Answer {
    /// How a hello that failed with `error` went, `socket_there` saying whether the broker's
    /// socket was there at the ask.
    fn of_failure(error: &aterm_uds::lan::LanError, socket_there: bool) -> Self {
        use aterm_uds::lan::{LanError, Unavailable};
        match error {
            LanError::Unavailable(Unavailable::NotInstalled) if socket_there => Self::Transient,
            LanError::Unavailable(Unavailable::Unchecked(_)) | LanError::Io(_) => Self::Transient,
            LanError::Unavailable(
                Unavailable::NotInstalled | Unavailable::Outdated { .. } | Unavailable::Refused(_),
            ) => Self::Settled,
        }
    }
}

/// What one ask found: how its hello went, and the stamp (`S`) of the broker's socket read just
/// before it (`None` where none was there).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Asked<S> {
    answer: Answer,
    socket: Option<S>,
}

/// One ask of [`watch_arrivals`]: the socket's stamp, read before the hello so that a socket made
/// again after the read is a later one, then the hello.
fn ask(socket: &std::path::Path) -> Asked<FileStamp> {
    let stamp = FileStamp::of(socket);
    let answer = match aterm_uds::lan::broker::hello() {
        Ok(hello) => Answer::Answered {
            direct: hello.direct.is_some(),
        },
        Err(error) => Answer::of_failure(&error, stamp.is_some()),
    };
    Asked {
        answer,
        socket: stamp,
    }
}

/// What [`arrivals`] keeps from one ask to the next.
struct Turns<S> {
    /// No ask yet: what answers at the first is no arrival.
    first: bool,
    /// The socket's stamp at the last ask, and whether the direct path answered it, where that ask
    /// was answered.
    answered: Option<(Option<S>, bool)>,
    /// When a [`Answer::Transient`] failure is asked again.
    retry: Option<Instant>,
    /// The wait after the next transient failure.
    delay: Duration,
}

impl<S: PartialEq> Turns<S> {
    const fn new() -> Self {
        Self {
            first: true,
            answered: None,
            retry: None,
            delay: RETRY_FIRST,
        }
    }

    /// Take what an ask at `now` found: whether it is an arrival, a hello answered where the ask
    /// before it was not, or answered from elsewhere: with the broker's socket made again since,
    /// or by the direct path where the broker answered (or the other way).
    fn asked(&mut self, now: Instant, asked: Asked<S>) -> bool {
        let answered = match asked.answer {
            Answer::Answered { direct } => Some((asked.socket, direct)),
            Answer::Settled | Answer::Transient => None,
        };
        let arrived = answered.is_some() && !self.first && self.answered != answered;
        self.first = false;
        if asked.answer == Answer::Transient {
            self.retry = Some(now + self.delay);
            self.delay = (self.delay * 2).min(RETRY_MOST);
        } else {
            self.retry = None;
            self.delay = RETRY_FIRST;
        }
        self.answered = answered;
        arrived
    }

    /// When to ask again with nothing heard, from `now`: while `hearing`, only a transient
    /// failure's retry; deaf, at the clock the answer calls for, or that retry if sooner.
    fn next(&self, now: Instant, hearing: bool) -> Option<Instant> {
        if hearing {
            return self.retry;
        }
        let poll = if self.answered.is_some() {
            PRESENT_POLL
        } else {
            ABSENT_POLL
        };
        let clock = now + poll;
        Some(self.retry.map_or(clock, |at| at.min(clock)))
    }
}

/// What [`arrivals`] rests on between asks.
trait Ears {
    /// Now, on the clock [`Self::rest`] waits by.
    fn now(&self) -> Instant;
    /// Whether file events tell what can move the answer.
    fn hears(&self) -> bool;
    /// Rest until something that can move the answer is heard (while [`Self::hears`]) or `until`
    /// comes (`None`: no deadline, only while hearing).
    fn rest(&mut self, until: Option<Instant>);
}

/// [`watch_arrivals`]' loop: `ask`, take what it found, call `arrived` on an arrival, then rest on
/// `ears` until the next ask is due, for `rounds` asks (`None`: forever).
fn arrivals<S: PartialEq>(
    mut ask: impl FnMut() -> Asked<S>,
    ears: &mut impl Ears,
    mut arrived: impl FnMut(),
    rounds: Option<usize>,
) {
    let mut turns = Turns::new();
    let mut asked = 0usize;
    while rounds.is_none_or(|rounds| asked < rounds) {
        asked += 1;
        let found = ask();
        if turns.asked(ears.now(), found) {
            arrived();
        }
        let until = turns.next(ears.now(), ears.hears());
        ears.rest(until);
    }
}

/// [`watch_arrivals`]' ears: a [`FileWatch`] of the broker socket's folder and of the allowance's
/// file, its tells on a channel; deaf where the watch did not start, and once its thread ended.
/// They are made once each key's first look told, so the first ask follows the watch: every tell
/// after it is a change since.
struct Watched {
    heard: Option<mpsc::Receiver<u64>>,
    _watch: Option<FileWatch>,
}

impl Watched {
    /// Watch `socket`'s folder and `record`, where there is one.
    fn start(socket: &std::path::Path, record: Option<std::path::PathBuf>) -> Self {
        let deaf = Self {
            heard: None,
            _watch: None,
        };
        let Some(folder) = socket.parent() else {
            aterm_log::info!(
                "lan arrivals: {} names no folder to watch: asking on a clock",
                socket.display()
            );
            return deaf;
        };
        let (told, heard) = mpsc::channel();
        let watch = FileWatch::platform(
            "lan-broker-events",
            Box::new(move |key| {
                let _ = told.send(key);
            }),
        );
        match watch {
            Ok(watch) => {
                watch.watch_directory(SOCKET_FOLDER, folder.to_path_buf());
                let mut unlooked = vec![SOCKET_FOLDER];
                if let Some(record) = record {
                    watch.watch(ALLOWANCE, record);
                    unlooked.push(ALLOWANCE);
                }
                // A watched key's first look always tells (`FileWatch`): the watch before the read.
                while !unlooked.is_empty() {
                    match heard.recv() {
                        Ok(key) => unlooked.retain(|&waiting| waiting != key),
                        Err(_) => {
                            aterm_log::info!(
                                "lan arrivals: the file watch ended at its first look: asking on a \
                                 clock"
                            );
                            return deaf;
                        }
                    }
                }
                Self {
                    heard: Some(heard),
                    _watch: Some(watch),
                }
            }
            Err(why) => {
                aterm_log::info!("lan arrivals: no file events ({why}): asking on a clock");
                deaf
            }
        }
    }
}

impl Ears for Watched {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn hears(&self) -> bool {
        self.heard.is_some()
    }

    fn rest(&mut self, until: Option<Instant>) {
        let Some(heard) = &self.heard else {
            if let Some(at) = until {
                std::thread::sleep(at.saturating_duration_since(Instant::now()));
            }
            return;
        };
        let got = match until {
            None => heard
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            Some(at) => heard.recv_timeout(at.saturating_duration_since(Instant::now())),
        };
        match got {
            // One ask answers every tell already queued.
            Ok(_) => while heard.try_recv().is_ok() {},
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                aterm_log::info!("lan arrivals: the file watch ended: asking on a clock");
                self.heard = None;
            }
        }
    }
}

#[cfg(test)]
#[path = "lan_reach_tests.rs"]
mod tests;
