// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};

use super::*;

thread_local! {
    static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static CLOCK: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

fn clock() -> Instant {
    CLOCK.with_borrow(|now| now.expect("test retry clock"))
}

fn record(words: std::fmt::Arguments<'_>) {
    LINES.with_borrow_mut(|lines| lines.push(words.to_string()));
}

struct NoBonjour;

impl Bonjour for NoBonjour {
    fn start(&self, _: &Request) -> Result<Box<dyn bonjour::Live>, Failure> {
        panic!("a connect uses no Bonjour operation")
    }
}

fn request(addr: SocketAddr) -> Request {
    Request::Connect {
        target: Target::Addr(addr),
        port: addr.port(),
        within_ms: 60_000,
        bound_if: None,
        source: None,
        hop_limit: None,
    }
}

/// The real request path shares a failed destination across clients. Even after the port
/// starts listening, its immediate retry returns the original error and writes no line.
/// NEGATIVE CONTROL: advancing its retry clock reaches the very same listening port. Even
/// an abandoned descriptor handoff ends the outage, so the next caller can connect at once.
/// This would fail if only an unused helper backed off or handoff failures poisoned peers.
#[test]
fn request_retries_use_the_shared_peer_clock_and_not_the_requesters_pid() {
    aterm_uds::lan::seam_here(
        aterm_uds::lan::Seam {
            loopback_is_local: true,
            ..aterm_uds::lan::Seam::default()
        },
        || {
            CLOCK.with_borrow_mut(|now| *now = Some(Instant::now()));
            let mut shared = Shared::new(
                Admission::SameUid,
                Box::new(NoBonjour),
                crate::store::Store::Unserved,
                record,
            );
            shared.clock = clock;
            let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let addr = target.local_addr().unwrap();
            drop(target);
            let (server, _client) = CtlStream::pair().unwrap();
            let before = answer(&shared, &server, request(addr), &mut || {}).unwrap_err();
            assert_eq!(before.0, libc::ECONNREFUSED);
            let target = TcpListener::bind(addr).unwrap();
            let (server, _client) = CtlStream::pair().unwrap();
            assert_eq!(
                answer(&shared, &server, request(addr), &mut || {}).unwrap_err(),
                before
            );
            LINES.with_borrow(|lines| assert_eq!(lines.len(), 1, "{lines:?}"));

            // The retry becomes due by advancing only the peer clock. A successful dial
            // followed by a vanished recipient clears the outage before the failed handoff.
            CLOCK.with_borrow_mut(|now| *now = Some(now.unwrap() + Duration::from_secs(1)));
            let (server, client) = CtlStream::pair().unwrap();
            let abandoning = std::thread::spawn(move || {
                let mut reader = Reader::new(1);
                assert!(matches!(
                    reader
                        .frame(&client, Some(Instant::now() + TAKE_HANG))
                        .unwrap(),
                    Some(Frame::Socket { .. })
                ));
                assert!(reader.take_fd().is_some());
            });
            assert!(answer(&shared, &server, request(addr), &mut || {}).is_err());
            abandoning.join().unwrap();
            LINES.with_borrow(|lines| {
                assert_eq!(lines.len(), 2, "{lines:?}");
                assert!(lines[1].ends_with("reachable"));
            });
            let (server, client) = CtlStream::pair().unwrap();
            let taking = std::thread::spawn(move || {
                let mut reader = Reader::new(1);
                assert!(matches!(
                    reader
                        .frame(&client, Some(Instant::now() + TAKE_HANG))
                        .unwrap(),
                    Some(Frame::Socket { .. })
                ));
                let socket = reader.take_fd().expect("the connected descriptor");
                wire::took(&client).unwrap();
                socket
            });
            answer(&shared, &server, request(addr), &mut || {}).unwrap();
            drop(taking.join().unwrap());
            drop(target);
            LINES.with_borrow(|lines| assert_eq!(lines.len(), 2));
        },
    );
}

/// **THE BROKER KEEPS THE LOG ITS PLIST NAMES, AND SERVES ON WHAT IT KEPT.** `keep_log` hands
/// `aterm_uds::job_log::keep` the file the broker's LaunchDaemon plist names for both outputs
/// (`LOG_PATH`), and `serve` takes the `Kept` it returns (`aterm lan serve` cannot serve without
/// calling it: nothing else makes one). Served here on a socket whose folder cannot be made, it
/// returns that error.
/// NEGATIVE CONTROL: this thread asked nothing before the call, and a test's run keeps nothing
/// (its output is not that file).
#[test]
fn the_broker_keeps_the_log_its_plist_names_and_serves_on_what_it_kept() {
    use aterm_uds::job_log::seam;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = scratch.path().join("a-file");
    std::fs::write(&file, "").unwrap();
    assert_eq!(seam::asked(), None);
    let kept = keep_log();
    let plist = seam::plist_outputs(&aterm_uds::lan::install::plist_text(Path::new(
        "/Applications/aterm.app",
    )));
    assert_eq!(seam::asked(), Some(plist.files()));
    assert_eq!(plist.files(), vec![PathBuf::from(aterm_uds::lan::LOG_PATH)]);
    assert!(
        !kept.stdout() && !kept.stderr(),
        "a test's output is no job log"
    );
    let refused = serve(
        Opts {
            source: scratch.path().join("aterm.app"),
            socket: file.join("broker.sock"),
        },
        kept,
    );
    assert!(refused.is_err(), "the socket's folder is a file");
}

/// The full name of [`a_burst_of_the_brokers_lines_stays_under_the_bound_where_its_log_is_kept`].
const BURST_CASE: &str =
    "server::tests::a_burst_of_the_brokers_lines_stays_under_the_bound_where_its_log_is_kept";

/// The line a client that keeps asking for an unreachable peer makes the broker write.
const UNREACHABLE: &str = "192.0.2.7:7681 could not connect: Connection refused (os error 61)";

/// **A BURST OF THE BROKER'S OWN LINES STAYS UNDER THE BOUND WHERE ITS LOG IS KEPT.** A
/// stand-in broker (this binary run again, its standard output and error on one file as its
/// plist's two keys naming one path open them) keeps that file and writes `log`'s line about
/// twice the cap's worth: the file stays under `job_log::CAP`, every line dated.
/// NEGATIVE CONTROL: the same burst with nothing kept grows the file past the cap.
#[test]
fn a_burst_of_the_brokers_lines_stays_under_the_bound_where_its_log_is_kept() {
    use aterm_uds::job_log::{CAP, seam, undated};
    if let Some((stage, dir)) = seam::stage() {
        let log_file = dir.join("aterm-lan.log");
        seam::open_as_service_manager(&seam::Outputs {
            stdout: Some(log_file.clone()),
            stderr: Some(log_file.clone()),
        })
        .unwrap();
        if stage == "kept" {
            assert!(aterm_uds::job_log::keep(&[&log_file]).stderr());
        }
        for _ in 0..seam::BURST {
            log(format_args!("{UNREACHABLE}"));
        }
        std::process::exit(0);
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let run = |stage: &str| {
        let dir = scratch.path().join(stage);
        std::fs::create_dir(&dir).unwrap();
        seam::rerun(BURST_CASE, stage, &dir).unwrap();
        std::fs::read_to_string(dir.join("aterm-lan.log")).unwrap()
    };
    let kept = run("kept");
    assert!(kept.len() as u64 <= CAP, "{} > {CAP}", kept.len());
    let line = format!("aterm-lan: {UNREACHABLE}");
    assert!(
        kept.lines()
            .skip(1)
            .all(|l| l != undated(l) && undated(l) == line),
        "every line dated"
    );
    assert!(run("unkept").len() as u64 > CAP, "the control is unbounded");
}

/// What the kernel says of a test's asker: a fixed cdhash and a bundle's executable.
fn loaded(_: &aterm_uds::peer::AuditToken) -> Option<([u8; 20], PathBuf)> {
    Some((
        [7; 20],
        PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
    ))
}

/// One connection to a broker that admits by a scripted full check (refusing where `refuse`),
/// from a client that sends its hello and `request` (none: a hello alone): the askers the broker
/// kept.
fn kept_of(refuse: bool, request: Option<Request>) -> Vec<crate::askers::Asker> {
    kept_speaking(aterm_uds::lan::PROTOCOL, refuse, request)
}

/// [`kept_of`], from a client whose hello names `protocol`.
fn kept_speaking(
    protocol: u32,
    refuse: bool,
    request: Option<Request>,
) -> Vec<crate::askers::Asker> {
    use aterm_uds::lan::wire::{self, Frame, Reader};
    let mut shared = Shared::new(
        Admission::Scripted(Scripted {
            token: Box::new(|_| Some(aterm_uds::peer::AuditToken([0, 0, 0, 0, 0, 4242, 0, 1]))),
            full_check: Box::new(move |_| {
                if refuse {
                    Err(CheckError::Refused("scripted".into()))
                } else {
                    Ok(())
                }
            }),
        }),
        Box::new(NoBonjour),
        crate::store::Store::Unserved,
        record,
    );
    let askers = Arc::new(crate::askers::Askers::default());
    shared.askers = Some(Arc::clone(&askers));
    shared.loaded = loaded;
    let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    wire::send(
        &client,
        &Frame::Hello {
            protocol,
            build: "asker".into(),
        },
    )
    .unwrap();
    if let Some(request) = request {
        wire::send(&client, &Frame::Request(request)).unwrap();
    }
    let worker = std::thread::spawn(move || connection_at(&shared, &server, Instant::now()));
    let said = Reader::new(0).frame(&client, Some(Instant::now() + Duration::from_secs(60)));
    assert!(
        matches!(
            (&said, refuse),
            (Ok(Some(Frame::Hello { .. })), false) | (Ok(Some(Frame::Refused { .. })), true)
        ),
        "{said:?}"
    );
    drop(client);
    worker.join().unwrap();
    askers.now()
}

/// An admitted client's `Consider` is kept, with the cdhash and executable the kernel names for
/// the token its admission checked; a hello alone, and a refused client's `Consider`, keep
/// nothing. NEGATIVE CONTROL (run): kept on every admission, the hello alone is kept too.
#[test]
fn only_an_admitted_consider_is_kept_as_an_asker() {
    assert_eq!(
        kept_of(false, Some(Request::Consider)),
        vec![crate::askers::Asker {
            uid: aterm_uds::peer::our_uid(),
            cdhash: [7; 20],
            bundle: PathBuf::from("/Applications/aterm.app"),
        }]
    );
    assert_eq!(kept_of(false, None), Vec::new(), "a hello alone");
    assert_eq!(
        kept_of(true, Some(Request::Consider)),
        Vec::new(),
        "a refused client"
    );
}

/// A newer build of another protocol is served nothing, but its ask to be considered is kept:
/// `Consider` keeps its tag (18) in every protocol, and this broker adopting that build is how its
/// protocol comes to be served. Its hello alone, and any other request (`SpotlightStore`, tag 17,
/// included), keep nothing; a refused client of another protocol keeps nothing. NEGATIVE
/// CONTROLS (run): with nothing read after another protocol's hello, its ask is never kept; with
/// the wire reading tag 17 as `Consider`, a `SpotlightStore` is kept.
#[test]
fn another_protocols_admitted_consider_is_kept() {
    let other = aterm_uds::lan::PROTOCOL + 1;
    assert_eq!(
        kept_speaking(other, false, Some(Request::Consider)),
        vec![crate::askers::Asker {
            uid: aterm_uds::peer::our_uid(),
            cdhash: [7; 20],
            bundle: PathBuf::from("/Applications/aterm.app"),
        }]
    );
    assert_eq!(
        kept_speaking(other, false, None),
        Vec::new(),
        "a hello alone"
    );
    assert_eq!(
        kept_speaking(
            other,
            false,
            Some(Request::Resolve {
                name: "m3.local".into(),
                within_ms: 10,
            })
        ),
        Vec::new(),
        "another request"
    );
    assert_eq!(
        kept_speaking(other, false, Some(Request::SpotlightStore)),
        Vec::new(),
        "a request for Spotlight's stores, tag 17, is no ask"
    );
    assert_eq!(
        kept_speaking(other, true, Some(Request::Consider)),
        Vec::new(),
        "a refused client"
    );
}

/// Root's record of this install failing is taken out only once the broker has served a peer it
/// admitted: what waits for that runs on no bind or registration, and on no hello of another
/// protocol; it runs at the first hello of this one, and nothing waits after it. NEGATIVE
/// CONTROLS (run): running it as it is registered takes the install's line out before any peer
/// was served; a hello that never runs it leaves the line for good.
#[cfg(target_os = "macos")]
#[test]
fn the_installs_record_is_taken_out_once_a_peer_was_served() {
    use aterm_types::macos_setup::refused;
    use aterm_uds::lan::install::{CodeDirectories, test_image};
    use aterm_uds::lan::{Seam, broker, seam_here};
    const MUST_HAPPEN: Duration = Duration::from_secs(60);
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("aterm.app");
    let code = source.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(code.parent().unwrap()).unwrap();
    let image = test_image(0x0100_000c, &[b"this build"]);
    std::fs::write(&code, &image).unwrap();
    let key = refused::broker_key(&source, &CodeDirectories::of_bytes(&image).unwrap()).unwrap();
    let record = dir.path().join("lan-broker.refused");
    std::fs::write(&record, format!("{key}\n")).unwrap();
    let socket = dir.path().join("run").join("broker.sock");
    let server = seam_here(Seam::default(), || {
        Server::bind(
            &socket,
            Admission::SameUid,
            None,
            Box::new(NoBonjour),
            crate::store::Store::Unserved,
        )
        .unwrap()
    });
    let (ran, runs) = std::sync::mpsc::channel();
    let at = record.clone();
    server.shared.after_first_hello(Box::new(move || {
        let _ = ran
            .send(crate::install_refused::served(&at, &source, &code).map_err(|e| e.to_string()));
    }));
    assert!(record.exists(), "a bound broker has served no one");
    let shared = Arc::clone(&server.shared);
    std::thread::spawn(move || {
        let _ = server.run();
    });

    let other = aterm_uds::connect::until(&socket, Instant::now() + MUST_HAPPEN).unwrap();
    let newer = Frame::Hello {
        protocol: aterm_uds::lan::PROTOCOL + 1,
        build: "a newer build".to_owned(),
    };
    wire::send(&other, &newer).unwrap();
    // Nothing more from it: its writing end closed, so the frame the broker reads after another
    // protocol's hello (an ask to be considered, if any) is read at once.
    other.shutdown(std::net::Shutdown::Write).unwrap();
    let deadline = Instant::now() + MUST_HAPPEN;
    while shared.counts.get(Count::WrongProtocol) == 0 {
        assert!(
            Instant::now() < deadline,
            "the other protocol's hello was never judged"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        shared.first_hello.lock().unwrap().is_some(),
        "a hello of another protocol served no one"
    );
    assert!(record.exists());

    let client = Seam {
        socket: Some(socket),
        ..Seam::default()
    };
    seam_here(client, broker::hello).unwrap();
    assert_eq!(runs.recv_timeout(MUST_HAPPEN), Ok(Ok(true)));
    assert!(
        !record.exists(),
        "the install's only line went, and the record with it"
    );
    assert!(shared.first_hello.lock().unwrap().is_none(), "it runs once");
}
