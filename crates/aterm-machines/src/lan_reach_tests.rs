// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(target_os = "macos")]
use aterm_uds::{AcceptWait, AcceptedMode};

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
        let (stream, _) =
            aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
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
