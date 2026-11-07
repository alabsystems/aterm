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
mod tests {
    use super::*;

    /// Only a vault on the LAN (a `.local` or single-label name, a link-local address or one on
    /// an attached subnet) is reached through the broker; a public URL and a path are not.
    /// NEGATIVE CONTROL: an `on_the_lan` true of every URL reads the public vault as on the LAN.
    #[test]
    fn only_a_url_on_the_lan_is_on_the_lan() {
        for (source, lan) in [
            ("http://alab-secrets.local:7484/v1/alab", true),
            ("http://m11:7484/v1/alab", true),
            ("http://169.254.10.20:7484/v1/alab", true),
            ("http://[fe80::1]:7484/v1/alab", true),
            ("https://user@m11.local/v1/alab", true),
            ("https://vault.example/v1/alab", false),
            ("https://8.8.8.8/v1/alab", false),
            ("/Volumes/Keys/team.vault", false),
            ("~/team.vault", false),
        ] {
            assert_eq!(on_the_lan(source), lan, "{source}");
        }
    }

    /// Scripted ears: each rest records its deadline (as an offset from the clock at the rest) and
    /// moves the clock to it, or by `heard` where nothing was due.
    struct Script {
        now: std::cell::Cell<Instant>,
        hearing: bool,
        heard: Duration,
        rests: Vec<Option<Duration>>,
    }

    impl Script {
        fn new(hearing: bool) -> Self {
            Self {
                now: std::cell::Cell::new(Instant::now()),
                hearing,
                heard: Duration::from_millis(250),
                rests: Vec::new(),
            }
        }
    }

    impl Ears for Script {
        fn now(&self) -> Instant {
            self.now.get()
        }

        fn hears(&self) -> bool {
            self.hearing
        }

        fn rest(&mut self, until: Option<Instant>) {
            let now = self.now.get();
            self.rests.push(until.map(|at| at - now));
            self.now.set(until.unwrap_or(now + self.heard));
        }
    }

    const fn answered(socket: u32) -> Asked<u32> {
        Asked {
            answer: Answer::Answered { direct: false },
            socket: Some(socket),
        }
    }

    const fn direct(socket: Option<u32>) -> Asked<u32> {
        Asked {
            answer: Answer::Answered { direct: true },
            socket,
        }
    }

    const fn not(answer: Answer, socket: Option<u32>) -> Asked<u32> {
        Asked { answer, socket }
    }

    /// Run the loop over `found` on `ears`: how many arrivals it called.
    fn run(found: &[Asked<u32>], ears: &mut Script) -> usize {
        let mut asks = found.iter().copied();
        let mut woken = 0;
        arrivals(
            || asks.next().unwrap(),
            ears,
            || woken += 1,
            Some(found.len()),
        );
        woken
    }

    /// Each turn from unanswered to answered is one arrival, and so is an answer from elsewhere
    /// than the last: a socket made again since (a broker restarted between two asks), or the
    /// direct path through a socket the broker left (it went between two asks); what answers at
    /// the first ask, and an answer from the same place again, are none.
    /// NEGATIVE CONTROL: comparing neither the socket nor who answered finds two arrivals, and the
    /// socket alone three; waking on every answer wakes eight times.
    #[test]
    fn an_answer_after_none_or_from_elsewhere_is_an_arrival_and_no_other() {
        let found = [
            answered(1),
            answered(1),
            not(Answer::Settled, None),
            answered(2),
            answered(3),
            answered(3),
            not(Answer::Transient, Some(3)),
            answered(3),
            direct(Some(3)),
            direct(Some(3)),
        ];
        assert_eq!(run(&found, &mut Script::new(true)), 4);
    }

    /// While file events are heard, a hello that answered or failed in a way only a heard change
    /// ends is asked again only when one is heard: no deadline at all.
    /// NEGATIVE CONTROL: resting on the old clocks while hearing names 30 s and 2 s deadlines.
    #[test]
    fn a_hearing_watch_asks_again_only_on_what_it_hears() {
        let mut ears = Script::new(true);
        run(
            &[
                answered(1),
                not(Answer::Settled, None),
                not(Answer::Settled, Some(4)),
                answered(4),
            ],
            &mut ears,
        );
        assert_eq!(ears.rests, [None, None, None, None]);
    }

    /// A hello that fails in a way nothing heard ends (a socket not yet listening, a hung
    /// exchange) is asked again from 1 s, doubling to 30 s, and the first answer or settled
    /// failure after it ends the retry and starts the next from 1 s.
    /// NEGATIVE CONTROL: treating a transient failure as settled rests with no deadline, and the
    /// broker that then listens is never asked.
    #[test]
    fn a_transient_failure_is_asked_again_on_a_back_off() {
        let mut ears = Script::new(true);
        let transient = not(Answer::Transient, Some(5));
        let mut found = vec![not(Answer::Settled, None)];
        found.extend([transient; 7]);
        found.extend([answered(5), transient]);
        let woken = run(&found, &mut ears);
        let secs = |s| Some(Duration::from_secs(s));
        assert_eq!(
            ears.rests,
            [
                None,
                secs(1),
                secs(2),
                secs(4),
                secs(8),
                secs(16),
                secs(30),
                secs(30),
                None,
                secs(1),
            ]
        );
        assert_eq!(woken, 1, "the socket that came to listen is an arrival");
    }

    /// Where no file events can be had the old clocks stand: every 2 s while none answers, every
    /// 30 s while one does, and a transient failure's retry where it is sooner.
    /// NEGATIVE CONTROL: resting with no deadline while deaf never asks again.
    #[test]
    fn a_deaf_watch_asks_on_the_old_clocks() {
        let mut ears = Script::new(false);
        let woken = run(
            &[
                not(Answer::Settled, None),
                answered(1),
                not(Answer::Transient, Some(1)),
                not(Answer::Transient, Some(1)),
                not(Answer::Transient, Some(1)),
            ],
            &mut ears,
        );
        let ms = |ms| Some(Duration::from_millis(ms));
        assert_eq!(
            ears.rests,
            [ms(2_000), ms(30_000), ms(1_000), ms(2_000), ms(2_000)]
        );
        assert_eq!(woken, 1);
    }

    /// A hello that found no broker is settled where no socket is there (one made is heard) and
    /// transient where one is (a broker between its bind and its listen); another protocol is
    /// settled (a broker that starts makes its socket again), and so is a refusal of this code; a
    /// broker that did not judge this process (serving its most connections, or its check did not
    /// finish) and an exchange's own failure are transient.
    #[test]
    fn a_failed_hello_is_settled_only_where_a_heard_change_ends_it() {
        use aterm_uds::lan::{LanError, Unavailable};
        let not_installed = || LanError::Unavailable(Unavailable::NotInstalled);
        assert_eq!(Answer::of_failure(&not_installed(), false), Answer::Settled);
        assert_eq!(
            Answer::of_failure(&not_installed(), true),
            Answer::Transient
        );
        let outdated = LanError::Unavailable(Unavailable::Outdated { ours: 1, theirs: 2 });
        assert_eq!(Answer::of_failure(&outdated, true), Answer::Settled);
        let refused = LanError::Unavailable(Unavailable::Refused("not aterm's code".into()));
        assert_eq!(Answer::of_failure(&refused, true), Answer::Settled);
        let busy = || {
            LanError::Unavailable(Unavailable::Unchecked(
                "the LAN broker is serving 128 connections".into(),
            ))
        };
        assert_eq!(Answer::of_failure(&busy(), true), Answer::Transient);
        assert_eq!(Answer::of_failure(&busy(), false), Answer::Transient);
        let hung = LanError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert_eq!(Answer::of_failure(&hung, true), Answer::Transient);
        assert_eq!(Answer::of_failure(&hung, false), Answer::Transient);
    }

    /// A hang detector, never a latency budget.
    #[cfg(target_os = "macos")]
    const MUST_HAPPEN: Duration = Duration::from_secs(60);

    /// The real loop and ears on a thread of their own under `seam` (and `direct`, where given),
    /// `record` the allowance's file they watch: each ask's answer, and each arrival, on a channel.
    /// The thread outlives the test, resting on what it hears in the test's own folder.
    #[cfg(target_os = "macos")]
    fn watching(
        seam: aterm_uds::lan::Seam,
        direct: Option<&'static dyn aterm_uds::lan::direct::Direct>,
    ) -> (mpsc::Receiver<Answer>, mpsc::Receiver<()>) {
        let (asked, answers) = mpsc::channel();
        let (woke, arrivals_heard) = mpsc::channel();
        std::thread::spawn(move || {
            let run = move || {
                let socket = aterm_uds::lan::dialled_socket();
                let mut ears = Watched::start(&socket, aterm_uds::lan::direct::record());
                assert!(ears.hears(), "the platform's file events");
                arrivals(
                    || {
                        let found = ask(&socket);
                        let _ = asked.send(found.answer);
                        found
                    },
                    &mut ears,
                    || {
                        let _ = woke.send(());
                    },
                    None,
                );
            };
            aterm_uds::lan::seam_here(seam, || match direct {
                Some(direct) => aterm_uds::lan::direct::seam_here(direct, run),
                None => run(),
            });
        });
        (answers, arrivals_heard)
    }

    /// A stand-in broker at `socket` that answers one hello in this build's protocol.
    #[cfg(target_os = "macos")]
    fn broker_at(socket: &std::path::Path) -> std::thread::JoinHandle<()> {
        let listener = aterm_uds::CtlListener::bind(socket).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = aterm_uds::accept(&listener).unwrap();
            answer_hello(&stream);
        })
    }

    /// Answer the hello on `stream` in this build's protocol, then read the client's.
    #[cfg(target_os = "macos")]
    fn answer_hello(stream: &aterm_uds::CtlStream) {
        use aterm_uds::lan::wire::{Frame, Reader, send};
        let hello = Frame::Hello {
            protocol: aterm_uds::lan::PROTOCOL,
            build: "arrival-stand-in".into(),
        };
        if send(stream, &hello).is_ok() {
            let _ = Reader::new(0).frame(stream, Some(Instant::now() + MUST_HAPPEN));
        }
    }

    /// A broker that starts once the watch rests (nothing at its socket at the first ask) is heard
    /// through its socket's folder, asked, and is an arrival, with no clock: the watch has no
    /// deadline while it hears. The first ask follows the folder's first look (`Watched::start`),
    /// so only the bind's own event can bring the ask that finds the broker.
    /// NEGATIVE CONTROL: a watch that does not hear the socket's folder (`Watched::start` without
    /// its `watch_directory`) never asks again, and the arrival misses its hang detector.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_broker_that_starts_under_the_watch_is_heard_and_is_an_arrival() {
        let root = aterm_tempfile::tempdir().unwrap();
        let socket = root.path().join("b.sock");
        let (answers, arrived) = watching(
            aterm_uds::lan::Seam {
                socket: Some(socket.clone()),
                ..aterm_uds::lan::Seam::default()
            },
            None,
        );
        assert_eq!(
            answers.recv_timeout(MUST_HAPPEN),
            Ok(Answer::Settled),
            "nothing at the socket at the first ask"
        );
        let served = broker_at(&socket);
        assert_eq!(
            arrived.recv_timeout(MUST_HAPPEN),
            Ok(()),
            "the broker that started is an arrival"
        );
        served.join().unwrap();
    }

    /// The direct path's allowance, read from the file `record` names: allowed once it says so.
    #[cfg(target_os = "macos")]
    struct AllowedWhenWritten {
        record: std::sync::OnceLock<std::path::PathBuf>,
    }

    #[cfg(target_os = "macos")]
    impl aterm_uds::lan::direct::Direct for AllowedWhenWritten {
        fn allowed(&self) -> Result<String, String> {
            let record = self.record.get().ok_or("no record")?;
            match std::fs::read_to_string(record) {
                Ok(said) if said.trim() == "allow" => Ok("the record's Allow".into()),
                _ => Err("no Allow in the record".into()),
            }
        }

        fn serve(&self, stream: aterm_uds::CtlStream) {
            answer_hello(&stream);
        }

        fn record(&self) -> Option<std::path::PathBuf> {
            self.record.get().cloned()
        }
    }

    /// With no broker installed, an Allow macOS records while aterm runs (to a program in a tab)
    /// is heard through its file, and the direct path that now answers is an arrival. The Allow
    /// is written after the first ask, which follows each key's first look (`Watched::start`), in
    /// a folder apart from the socket's, so only the allowance's own events can bring the ask that
    /// finds it.
    /// NEGATIVE CONTROL: a watch that does not hear the allowance's file (`Watched::start`
    /// without its `watch`) never asks again, and the arrival misses its hang detector.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_allowance_recorded_under_the_watch_is_heard_and_is_an_arrival() {
        static DIRECT: AllowedWhenWritten = AllowedWhenWritten {
            record: std::sync::OnceLock::new(),
        };
        let root = aterm_tempfile::tempdir().unwrap();
        for folder in ["lan", "prefs"] {
            std::fs::create_dir(root.path().join(folder)).unwrap();
        }
        let record = root.path().join("prefs/networkextension.plist");
        std::fs::write(&record, "ask\n").unwrap();
        DIRECT.record.set(record.clone()).unwrap();
        let (answers, arrived) = watching(
            aterm_uds::lan::Seam {
                socket: Some(root.path().join("lan/b.sock")),
                ..aterm_uds::lan::Seam::default()
            },
            Some(&DIRECT),
        );
        assert_eq!(
            answers.recv_timeout(MUST_HAPPEN),
            Ok(Answer::Settled),
            "no broker and no Allow at the first ask"
        );
        std::fs::write(&record, "allow\n").unwrap();
        assert_eq!(
            arrived.recv_timeout(MUST_HAPPEN),
            Ok(()),
            "the direct path the Allow opened is an arrival"
        );
    }
}
