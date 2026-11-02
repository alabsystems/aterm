// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The drive's control connections against a socket that never answers: every exchange ends at
//! its bound ([`crate::harness::Timings::drive_reply`]), what was left unanswered is asked again
//! on the caller's own retry, and no roster read holds the sweep lock.

use super::super::relaunch::{CAUSE_HOST, resume_as, snapshot_with_claim};
use super::*;
use crate::harness::{FakeInstanceLog, UNANSWERED, fake_instance};
use crate::supervise::ScratchDir;
use aterm_spec::wait::MUST_HAPPEN;

const TAB: &str = "s-0ddba11c0ffee0ddba11";
const SESSION: &str = "0ddba11c-1111-2222-3333-444455556666";
const TOKEN: &str = "hang-tok";
/// The bound a test gives an exchange it leaves unanswered, waited out in full.
const UNANSWERED_BOUND: Duration = Duration::from_millis(200);
/// No process has this pid.
const NO_PID: u32 = 0x7fff_fff0;

#[test]
fn a_not_connected_probe_error_on_a_live_peer_cannot_refresh_authentication() {
    thread_local! {
        static ARM_FAILURE: std::cell::Cell<(usize, usize)> = const {
            std::cell::Cell::new((0, 0))
        };
    }
    // Refuse once before the request begins, then once during its first write. The
    // latter must not mistake our own error cleanup for a peer authentication refusal.
    for fail_at in [1, 2] {
        let (near, _far) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
        let mut client = RelayClient::new(near).bounded(MUST_HAPPEN);
        ARM_FAILURE.with(|state| state.set((0, fail_at)));
        client.bound = Some((
            |_, _| {
                ARM_FAILURE.with(|state| {
                    let (calls, fail_at) = state.get();
                    state.set((calls + 1, fail_at));
                    if calls + 1 == fail_at {
                        Err(std::io::ErrorKind::NotConnected.into())
                    } else {
                        Ok(())
                    }
                })
            },
            MUST_HAPPEN,
        ));
        let mut recovered = false;
        let result = probe_client(client, || {
            recovered = true;
            Err(std::io::Error::other("unexpected authentication fallback"))
        });
        let error = result.err().expect("a live-peer probe cannot recover");
        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert!(!recovered, "failed at timeout arm {fail_at}");
    }
}

#[test]
fn an_unread_probe_cannot_refresh_authentication_after_its_peer_closes() {
    let (near, far) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    far.shutdown(std::net::Shutdown::Both).unwrap();
    drop(far);
    let mut client = RelayClient::new(near).bounded(MUST_HAPPEN);
    client.unread = true;
    let mut recovered = false;
    let result = probe_client(client, || {
        recovered = true;
        Err(std::io::Error::other("unexpected authentication fallback"))
    });
    assert_eq!(
        result
            .err()
            .expect("an unread client cannot recover")
            .kind(),
        std::io::ErrorKind::NotConnected
    );
    assert!(!recovered);
}

/// A peer can shut both halves while a descriptor still exists. A write then fails
/// even though its last reply bytes must still prevent authentication recovery.
#[test]
fn a_shut_peer_with_a_retained_descriptor_preserves_its_partial_reply() {
    use std::io::{BufRead as _, Write as _};
    thread_local! {
        static ARMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    for (reply, fail_write) in [
        (b"OK partial".as_slice(), false),
        (b"".as_slice(), false),
        (b"OK partial".as_slice(), true),
        (b"".as_slice(), true),
    ] {
        let (near, mut far) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
        far.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        let mut client = RelayClient::authenticated(near, "old-token")
            .unwrap()
            .bounded(MUST_HAPPEN);
        let mut auth = String::new();
        std::io::BufReader::new(&far).read_line(&mut auth).unwrap();
        assert_eq!(auth, "AUTH old-token\n");
        far.write_all(reply).unwrap();
        far.shutdown(std::net::Shutdown::Both).unwrap();
        #[cfg(target_os = "macos")]
        assert!(!aterm_uds::hangup::peer_closed(client.transport()));
        if fail_write {
            // Force the pre-read failure in this socket state independently of
            // whether this kernel accepts a write after the peer's read shutdown.
            ARMS.with(|calls| calls.set(0));
            client.bound = Some((
                |_, _| {
                    ARMS.with(|calls| {
                        let call = calls.get() + 1;
                        calls.set(call);
                        if call == 2 {
                            Err(std::io::ErrorKind::BrokenPipe.into())
                        } else {
                            Ok(())
                        }
                    })
                },
                MUST_HAPPEN,
            ));
        }
        let (replacement, _server) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
        let mut recovered = false;
        let result = probe_client(client, || {
            recovered = true;
            Ok(RelayClient::new(replacement))
        });
        assert_eq!(recovered, reply.is_empty(), "reply {reply:?}, {fail_write}");
        assert_eq!(
            result.is_ok(),
            reply.is_empty(),
            "reply {reply:?}, {fail_write}"
        );
        drop(far);
    }
}

/// Close the authenticated peer before the probe starts, without depending on which thread
/// wins the race between AUTH and timeout arming. Queued reply bytes forbid auth recovery.
#[test]
fn a_peer_closed_before_the_probe_can_refresh_its_authentication() {
    use std::io::{BufRead as _, Write as _};
    use std::net::Shutdown;
    for reply in [b"OK partial".as_slice(), b""] {
        let (near, mut far) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
        far.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        let c = RelayClient::authenticated(near, "old-token")
            .unwrap()
            .bounded(crate::DEFAULT_REPLY_WAIT);
        let mut line = String::new();
        std::io::BufReader::new(&far).read_line(&mut line).unwrap();
        assert_eq!(line, "AUTH old-token\n");
        far.write_all(reply).unwrap();
        far.shutdown(Shutdown::Both).unwrap();
        drop(far);
        aterm_spec::wait::until_peer_closed(c.transport());
        let (replacement, _server) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
        let mut recovered = false;
        let result = probe_client(c, || {
            recovered = true;
            Ok(RelayClient::new(replacement))
        });
        if reply.is_empty() {
            assert!(
                result.is_ok(),
                "closed auth peer: {:?}",
                result.as_ref().err()
            );
            assert!(recovered);
        } else {
            assert!(
                result.is_err(),
                "partial reply became successful auth recovery"
            );
            assert!(!recovered);
        }
    }
}

/// A real socket keeps listening while its first connection closes, so an erroneous token
/// fallback would really succeed and be observable as a second AUTH, not fail at connect.
#[test]
fn partial_probe_replies_cannot_succeed_through_authentication_fallback() {
    use std::io::{BufRead as _, Write as _};
    use std::net::Shutdown;
    use std::os::unix::net::UnixListener;
    type ProbeCase = (
        &'static str,
        &'static [u8],
        bool,
        bool,
        Option<std::io::ErrorKind>,
        bool,
    );
    let cases: &[ProbeCase] = &[
        (
            "cut-head",
            b"OK partial",
            false,
            false,
            Some(std::io::ErrorKind::UnexpectedEof),
            false,
        ),
        (
            "cut-body",
            b"OK 2\none\n",
            false,
            false,
            Some(std::io::ErrorKind::UnexpectedEof),
            false,
        ),
        (
            "bad-count",
            b"OK 99999999999999999999999999\n",
            false,
            false,
            Some(std::io::ErrorKind::InvalidData),
            false,
        ),
        ("auth-error", b"ERR auth\n", false, false, None, true),
        ("auth-close", b"", true, false, None, true),
        ("answered", b"OK version 0\n", false, false, None, false),
        ("auth-full-close", b"", true, true, None, true),
        (
            "cut-head-full-close",
            b"OK partial",
            false,
            true,
            Some(std::io::ErrorKind::UnexpectedEof),
            false,
        ),
    ];
    for &(name, reply, early_close, full_close, error, fallback) in cases {
        let dir = ScratchDir::new("drive-probe", name);
        let sock = dir.join("c.sock").to_str().unwrap().to_string();
        let listener = aterm_uds::kept_out(|| UnixListener::bind(&sock)).unwrap();
        let token_file = aterm_uds::latest::token_path_for_sock(&sock).unwrap();
        std::fs::write(token_file, "beside-token\n").unwrap();
        let serving = aterm_uds::kept_out(|| listener.try_clone()).unwrap();
        let reply = reply.to_vec();
        let (done, ended) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = aterm_uds::accept(&serving).unwrap();
            stream.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
            stream.set_write_timeout(Some(MUST_HAPPEN)).unwrap();
            let mut reader = std::io::BufReader::new(&stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "AUTH supplied-token\n");
            if !early_close {
                line.clear();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "version\n");
            }
            drop(reader);
            stream.write_all(&reply).unwrap();
            if full_close {
                let _ = stream.shutdown(Shutdown::Both);
                drop(stream);
                done.send(()).unwrap();
            } else {
                // Keep the receiving half alive until the client returns, so these cases
                // deterministically exercise EOF rather than a full peer disconnection.
                if let Err(error) = stream.shutdown(Shutdown::Write) {
                    // A complete reply can let the client close before this half-close.
                    assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
                    assert!(aterm_uds::hangup::peer_closed(&stream));
                }
                done.send(()).unwrap();
                released.recv_timeout(MUST_HAPPEN).unwrap();
            }
        });
        let result = harness_client_within(
            &sock,
            Some(&HarnessToken::new("supplied-token")),
            crate::DEFAULT_REPLY_WAIT,
        );
        if !full_close {
            release.send(()).unwrap();
        }
        if let Some(kind) = error {
            let failure = result.err().expect("invalid probe must fail");
            assert_eq!(failure.kind(), kind, "{name}: {failure}");
        } else {
            assert!(result.is_ok(), "{name}: {:?}", result.as_ref().err());
        }
        ended
            .recv_timeout(MUST_HAPPEN)
            .expect("server closes within hang detector");
        worker.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        match aterm_uds::kept_out(|| listener.accept()) {
            Ok((stream, _)) => {
                assert!(fallback, "{name}: invalid probe attempted fallback");
                stream.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
                let mut auth = String::new();
                std::io::BufReader::new(stream)
                    .read_line(&mut auth)
                    .unwrap();
                assert_eq!(auth, "AUTH beside-token\n");
            }
            Err(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock);
                assert!(!fallback, "{name}: legitimate auth fallback was lost");
            }
        }
    }
}

/// A sweep's options over `dir` dialling `sock` with the window's token, each exchange bounded
/// by `bound`.
fn dialling(dir: &Path, sock: &str, bound: Duration) -> Opts {
    Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: Some(sock.to_string()),
        token: Some(HarnessToken::new(TOKEN)),
        only_sid: Some(TAB.to_string()),
        dry_run: false,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        tell: Tell::Off,
        press: None,
        timings: crate::harness::Timings {
            drive_reply: bound,
            ..crate::harness::Timings::FAST
        },
    }
}

/// A stand-in for the window's control socket at `<dir>/h.sock`, answering what `answer` names
/// and leaving every other request unanswered ([`UNANSWERED`]).
fn instance(
    dir: &Path,
    answer: impl Fn(&str) -> Option<String> + Send + 'static,
) -> (String, FakeInstanceLog) {
    std::fs::create_dir_all(dir).expect("the instance's folder");
    fake_instance(&dir.join("h.sock"), TOKEN, move |line| {
        answer(line).unwrap_or_else(|| UNANSWERED.to_string())
    })
}

/// An instance's answer to its `version` probe alone.
fn version(line: &str) -> Option<String> {
    (line == "version").then(|| "OK version 0".to_string())
}

/// `who` naming [`TAB`] in front of the group `fg` (none at `0`).
fn who(fg: u32) -> String {
    format!("OK 1\n1 {TAB} fgpgid={fg}")
}

/// What `work` answers on its own thread, waited for at most the one-minute hang detector; a
/// build it would probe starts, as the drive tests' sweeps find.
fn within_detector<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        build_start::PROBE.with(|named| {
            *named.borrow_mut() = Some(std::rc::Rc::new(|_: &Path| build_start::Probe::Starts));
        });
        let _ = tx.send(work());
    });
    rx.recv_timeout(MUST_HAPPEN)
        .unwrap_or_else(|_| panic!("{what}: no answer within the hang detector"))
}

/// The requests `asked` holds once it holds `line`, waited for at most the hang detector.
fn heard(asked: &FakeInstanceLog, line: &str) -> Vec<String> {
    let until = Instant::now() + MUST_HAPPEN;
    loop {
        let now = asked.lock().map(|a| a.clone()).unwrap_or_default();
        if now.iter().any(|l| l == line) {
            return now;
        }
        assert!(
            Instant::now() < until,
            "{line:?} never reached the instance: {now:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The window's socket never answering the drive's `version` probe is no connection, within the
/// bound, and never the token beside the socket; an answered probe is the connection the roster
/// is read on (lanes:H1).
#[test]
fn a_probe_the_window_never_answers_ends_at_its_bound() {
    let dir = ScratchDir::new("upgrade", "hang-probe");
    let (mute, asked) = instance(&dir.join("mute"), |_| None);
    let opts = dialling(&dir, &mute, UNANSWERED_BOUND);
    let connected = within_detector("the probe", move || connect_at(&opts, &mute).map(drop));
    assert!(connected.is_err(), "an unanswered probe connects nowhere");
    assert_eq!(heard(&asked, "version"), ["version"]);

    let (live, asked) = instance(&dir.join("live"), |l| {
        version(l).or_else(|| (l == "who").then(|| who(42)))
    });
    let opts = dialling(&dir, &live, crate::DEFAULT_REPLY_WAIT);
    let tabs = within_detector("the roster", move || {
        connect_at(&opts, &live)
            .ok()
            .and_then(|mut c| host_roster(&mut c))
    });
    assert_eq!(
        tabs,
        Some(vec![LiveTab {
            sid: TAB.to_string(),
            fgpgid: Some(42)
        }]),
        "a prompt answer lands and is read"
    );
    assert_eq!(heard(&asked, "who"), ["version", "who"]);
}

/// A step whose tab's `status` the window never answers waits, within the bound, and asks
/// nothing more on that connection; answered, its roster is read (host-step:R1).
#[test]
fn a_step_whose_status_is_never_answered_waits() {
    let dir = ScratchDir::new("upgrade", "hang-step");
    let (mute, asked) = instance(&dir.join("mute"), version);
    let opts = dialling(&dir, &mute, UNANSWERED_BOUND);
    let stepped = within_detector("the step", move || step(&opts).step);
    assert_eq!(stepped, "wait:tab-not-live");
    let status = format!("@{TAB} status");
    assert_eq!(heard(&asked, &status), ["version".to_string(), status]);

    let (live, _asked) = instance(&dir.join("live"), |l| {
        version(l)
            .or_else(|| l.ends_with(" status").then(|| "OK schema=1".to_string()))
            .or_else(|| (l == "who").then(|| who(0)))
    });
    let opts = dialling(&dir, &live, crate::DEFAULT_REPLY_WAIT);
    let stepped = within_detector("the answered step", move || step(&opts).step);
    assert_ne!(
        stepped, "wait:tab-not-live",
        "an answered roster names the tab live"
    );
}

/// A snapshot whose roster the window never answers is unread, within the bound; an answered
/// roster is what it refuses on (lanes:H3).
#[test]
fn a_snapshot_whose_roster_is_never_answered_is_unread() {
    let dir = ScratchDir::new("upgrade", "hang-snapshot");
    let (mute, _asked) = instance(&dir.join("mute"), version);
    let opts = dialling(&dir, &mute, UNANSWERED_BOUND);
    let snap = within_detector("the snapshot", move || {
        snapshot_with_claim(&opts, None).err()
    });
    assert!(snap.is_some(), "an unanswered roster is no snapshot");

    let (live, _asked) = instance(&dir.join("live"), |l| {
        version(l).or_else(|| (l == "who").then(|| who(0)))
    });
    let opts = dialling(&dir, &live, crate::DEFAULT_REPLY_WAIT);
    let snap = within_detector("the answered snapshot", move || {
        snapshot_with_claim(&opts, None).err()
    });
    assert_eq!(snap, Some("no-foreground"), "the answered roster was read");
}

/// The host's look at the upgrades ends within the bound when the window never answers its
/// roster or a tab's `meta unset attention`; the tab left unsettled is asked again at the next
/// look, and once settled is asked nothing more (lanes:H4).
#[test]
fn a_look_the_window_never_answers_ends_and_asks_again() {
    let dir = ScratchDir::new("upgrade", "hang-view");
    let mut view = View::default();
    let (mute, _asked) = instance(&dir.join("mute"), version);
    let opts = dialling(&dir, &mute, UNANSWERED_BOUND);
    let (mut view, next) = within_detector("the unanswered roster's look", move || {
        let next = view.refresh_keeping(&opts, &mut |_, _| {});
        (view, next)
    });
    assert!(next.is_some(), "an unread roster is looked at again");

    let (held, asked) = instance(&dir.join("held"), |l| {
        version(l).or_else(|| (l == "who").then(|| who(0)))
    });
    let opts = dialling(&dir, &held, UNANSWERED_BOUND);
    let settle = format!("@{TAB} meta unset attention owner={ATTENTION_OWNER}");
    let mut view = within_detector("the unanswered settle's look", move || {
        view.refresh_keeping(&opts, &mut |_, _| {});
        view
    });
    assert!(heard(&asked, &settle).contains(&settle));

    let (live, asked) = instance(&dir.join("live"), |l| {
        version(l)
            .or_else(|| (l == "who").then(|| who(0)))
            .or_else(|| {
                l.contains(" meta unset attention ")
                    .then(|| "OK".to_string())
            })
    });
    let opts = dialling(&dir, &live, crate::DEFAULT_REPLY_WAIT);
    let looked = opts.clone();
    let mut view = within_detector("the answered look", move || {
        view.refresh_keeping(&looked, &mut |_, _| {});
        view
    });
    assert!(heard(&asked, &settle).contains(&settle), "asked again");
    within_detector("the settled look", move || {
        view.refresh_keeping(&opts, &mut |_, _| {});
    });
    let settles = asked
        .lock()
        .map(|a| a.iter().filter(|l| **l == settle).count())
        .unwrap_or_default();
    assert_eq!(settles, 1, "a settled tab is asked nothing more");
}

/// A carry-on the window never answers ends within the bound and releases the sweep lock, and
/// asks nothing more on a connection whose reply never came (lanes:H5, host-step:R2).
#[test]
fn a_carry_the_window_never_answers_releases_the_sweep() {
    let dir = ScratchDir::new("upgrade", "hang-carry");
    let (mute, asked) = instance(&dir.join("mute"), version);
    let opts = dialling(&dir, &mute, UNANSWERED_BOUND);
    let st = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: NO_PID,
        shell: std::process::id(),
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        cause: CAUSE_HOST.to_string(),
        ..St::default()
    };
    save(&opts, SESSION, &st);
    let carried = opts.clone();
    let r = within_detector("the carry", move || resume_as(&carried, false, None));
    assert_ne!(r.step, "current", "{r:?}");
    assert!(
        sweep_lock(&opts).is_ok_and(|held| held.is_some()),
        "the sweep lock is free once the carry ends"
    );
    let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
    let probes = asked.iter().filter(|l| *l == "version").count();
    assert!(
        asked.len() <= 2 * probes,
        "one unanswered request a connection, never one after it: {asked:?}"
    );
}

/// Noting a session behind reads the roster before it takes the sweep lock: while the window
/// holds that read, every other session's sweep goes on, and an unanswered roster is unread
/// within the bound (lanes:H7, host-step:R2).
#[test]
fn noting_behind_never_holds_the_sweep_across_its_roster_read() {
    let dir = ScratchDir::new("upgrade", "hang-behind");
    let (mute, _asked) = instance(&dir.join("mute"), version);
    let opts = dialling(&dir, &mute, UNANSWERED_BOUND);
    let behind = within_detector("the note", move || note_behind(&opts, 0));
    assert_eq!(behind, Behind::Unread(UNREAD_SOCKET));

    let (held, asked) = instance(&dir.join("held"), version);
    let opts = dialling(&dir, &held, crate::DEFAULT_REPLY_WAIT);
    let noted = opts.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        build_start::PROBE.with(|named| {
            *named.borrow_mut() = Some(std::rc::Rc::new(|_: &Path| build_start::Probe::Starts));
        });
        let _ = tx.send(note_behind(&noted, 0));
    });
    heard(&asked, "who");
    let free = sweep_lock(&opts).map(|held| held.is_some());
    drop(asked);
    let behind = rx.recv_timeout(MUST_HAPPEN);
    assert_eq!(free, Ok(true), "the roster read holds no sweep lock");
    assert_eq!(
        behind,
        Ok(Behind::Unread(UNREAD_SOCKET)),
        "a roster the window closed on is unread"
    );
}

/// The claims read's own bound ([`CLAIMS_WAIT`]) covers the probe before it, whatever the drive
/// bound: a window that never answers the probe is no roster (lanes:H9).
#[test]
fn the_claims_read_bounds_its_probe() {
    let dir = ScratchDir::new("upgrade", "hang-claims");
    let (mute, _asked) = instance(&dir.join("mute"), |_| None);
    let opts = dialling(&dir, &mute, 2 * MUST_HAPPEN);
    let tabs = within_detector("the claims read", move || instance_tabs(&opts));
    assert_eq!(tabs, Vec::new());

    let (live, _asked) = instance(&dir.join("live"), |l| {
        version(l).or_else(|| (l == "who").then(|| who(42)))
    });
    let opts = dialling(&dir, &live, 2 * MUST_HAPPEN);
    let tabs = within_detector("the answered claims read", move || instance_tabs(&opts));
    assert_eq!(
        tabs,
        vec![LiveTab {
            sid: TAB.to_string(),
            fgpgid: Some(42)
        }]
    );
}

/// Noting a session behind takes the sweep lock for the mint alone: a note that mints nothing
/// reads the roster, the agents' homes and the builds while another sweep holds the lock, and
/// answers what it answers with the lock free, never busy (review F4).
#[test]
fn a_note_that_mints_nothing_never_waits_on_the_sweep() {
    let dir = ScratchDir::new("upgrade", "hang-behind-mint");
    let (live, _asked) = instance(&dir.join("live"), |l| {
        version(l).or_else(|| (l == "who").then(|| who(0)))
    });
    let opts = dialling(&dir, &live, crate::DEFAULT_REPLY_WAIT);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("an empty roster");
    let noted = opts.clone();
    let free = within_detector("the note, the lock free", move || note_behind(&noted, 0));
    assert_ne!(free, Behind::Busy);
    let held = sweep_lock(&opts)
        .ok()
        .flatten()
        .expect("PRECONDITION: the test holds the sweep lock");
    let noted = opts.clone();
    let busy = within_detector("the note, the lock held", move || note_behind(&noted, 0));
    drop(held);
    assert_eq!(busy, free, "the reads wait on no sweep");
}
