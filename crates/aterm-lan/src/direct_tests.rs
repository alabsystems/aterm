// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::Mutex;
use std::time::Duration;

use aterm_uds::lan::wire::Target;
use aterm_uds::lan::{ConnectOptions, LanError, Seam, Unavailable, broker, direct, seam_here};

use super::*;

/// A hang detector, never a latency budget.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// The account the fixtures' configuration is for.
const GUEST_USER: &str = "E8D0C90E-068E-4C80-AC44-6815AE2AC56B";

/// The direct path as this test binary's code sees it: macOS's record read from `plist` (a
/// fixture), the code named as released 0.158.0's run by the fixtures' account, and macOS
/// answering for this process as that code (the responsible app's stand-in).
fn path_reading(plist: &'static str) -> &'static Direct {
    path_answered_as(plist, |_| Ok(()))
}

fn path_answered_as(
    plist: &'static str,
    responsible: fn(&grant::Code) -> Result<(), String>,
) -> &'static Direct {
    path_of(plist, responsible, GUEST_USER, quiet)
}

fn path_of(
    plist: &'static str,
    responsible: fn(&grant::Code) -> Result<(), String>,
    user: &str,
    log: fn(std::fmt::Arguments<'_>),
) -> &'static Direct {
    let direct = Box::leak(Box::new(Direct {
        code: OnceLock::new(),
        shared: OnceLock::new(),
        plist,
        responsible,
        log,
    }));
    let _ = direct.code.set(Ok(grant::Code {
        identifier: "com.aterm.aterm".into(),
        team: Some("A66A9P66Z7".into()),
        executable: "/Applications/aterm.app/Contents/MacOS/aterm".into(),
        user: user.into(),
    }));
    direct
}

/// The lines [`record`] heard: one test's server log.
static RECORDED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn record(line: std::fmt::Arguments<'_>) {
    RECORDED.lock().unwrap().push(line.to_string());
}

fn fixture(name: &str) -> &'static str {
    let path = aterm_tempfile::manifest_dir()
        .join("src/fixtures")
        .join(name);
    Box::leak(path.to_string_lossy().into_owned().into_boxed_str())
}

/// No broker at the seam's socket, loopback classified as local network.
fn no_broker(root: &aterm_tempfile::TempDir) -> Seam {
    Seam {
        loopback_is_local: true,
        socket: Some(root.path().join("absent").join("broker.sock")),
        ..Seam::default()
    }
}

/// With no broker installed and macOS's allowance for this code on record, the broker's
/// protocol is served in-process: a hello says so, and a LAN connect comes back connected.
#[test]
fn no_broker_and_an_allowance_serve_this_process_itself() {
    let root = aterm_tempfile::tempdir().unwrap();
    let allowed = path_reading(fixture("nehelper-granted.plist"));
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = target.local_addr().unwrap();
    let (hello, mut stream) = seam_here(no_broker(&root), || {
        direct::seam_here(allowed, || {
            let hello = broker::hello().unwrap();
            let (stream, reached) = broker::connect(
                &Target::Addr(addr),
                addr.port(),
                MUST_HAPPEN,
                ConnectOptions::default(),
            )
            .unwrap();
            assert_eq!(reached, addr);
            (hello, stream)
        })
    });
    assert_eq!(hello.protocol, aterm_uds::lan::PROTOCOL);
    assert_eq!(
        hello.direct.as_deref(),
        Some("macOS allowed com.aterm.aterm (team A66A9P66Z7) on the local network")
    );
    let (mut far, _) = aterm_uds::accept(
        &target,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    stream.write_all(b"ping").unwrap();
    let mut got = [0u8; 4];
    far.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
    far.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"ping");
}

/// NEGATIVE CONTROLS: the same process with no allowance on record (macOS asked and heard
/// nothing; a dismissed question; an unreadable record), or one another app answers for, has
/// no direct path and reads NotInstalled, as a build with none does; and a broker that answers
/// is the broker, even beside an allowance: its answer stands.
#[test]
fn without_an_allowance_or_beside_a_broker_there_is_no_direct_path() {
    let root = aterm_tempfile::tempdir().unwrap();
    for plist in [
        fixture("nehelper-unanswered.plist"),
        fixture("nehelper-dismissed.plist"),
        Box::leak(
            root.path()
                .join("missing.plist")
                .to_string_lossy()
                .into_owned()
                .into_boxed_str(),
        ),
    ] {
        let reading = path_reading(plist);
        let got = seam_here(no_broker(&root), || {
            direct::seam_here(reading, broker::hello)
        });
        assert!(
            matches!(got, Err(LanError::Unavailable(Unavailable::NotInstalled))),
            "{plist}: {got:?}"
        );
    }
    // macOS's allowance on record, and this process answered for by another app (an `aterm`
    // typed in Terminal): no direct path, since that app's question is not aterm's.
    let terminal = path_answered_as(fixture("nehelper-granted.plist"), |_| {
        Err("macOS answers this process's LAN use as Terminal's".to_owned())
    });
    let got = seam_here(no_broker(&root), || {
        direct::seam_here(terminal, broker::hello)
    });
    assert!(
        matches!(got, Err(LanError::Unavailable(Unavailable::NotInstalled))),
        "{got:?}"
    );
    // macOS's allowance on record for another account of this Mac: this account's own
    // question is still unanswered, so no direct path.
    let other_account = path_of(
        fixture("nehelper-granted.plist"),
        |_| Ok(()),
        "5D773287-9015-4E7D-A8F3-7CE6088E62D5",
        quiet,
    );
    let got = seam_here(no_broker(&root), || {
        direct::seam_here(other_account, broker::hello)
    });
    assert!(
        matches!(got, Err(LanError::Unavailable(Unavailable::NotInstalled))),
        "{got:?}"
    );
    // A broker that speaks another protocol at the socket: Outdated, though macOS allowed
    // this code.
    let socket = root.path().join("run").join("broker.sock");
    let server =
        crate::server::Server::bind(&socket, Admission::SameUid, crate::bonjour::system()).unwrap();
    std::thread::spawn(move || {
        let _ = server.run();
    });
    let allowed = path_reading(fixture("nehelper-granted.plist"));
    let newer = Seam {
        socket: Some(socket),
        protocol: Some(aterm_uds::lan::PROTOCOL + 1),
        ..Seam::default()
    };
    let got = seam_here(newer, || direct::seam_here(allowed, broker::hello));
    assert!(
        matches!(
            got,
            Err(LanError::Unavailable(Unavailable::Outdated { .. }))
        ),
        "{got:?}"
    );
}

/// A request the direct path fails reaches its client as the broker's error, and the server's
/// line for it goes to the path's own log, never the broker's stderr: the production path's
/// log is [`quiet`], so nothing lands on the person's terminal. NEGATIVE CONTROL: with the
/// connection's lines written by the broker's own log again, the recorder hears nothing.
#[test]
fn a_failed_direct_request_is_the_clients_alone_to_say() {
    let root = aterm_tempfile::tempdir().unwrap();
    let recorded = path_of(
        fixture("nehelper-granted.plist"),
        |_| Ok(()),
        GUEST_USER,
        record,
    );
    let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = closed.local_addr().unwrap();
    drop(closed);
    let got = seam_here(no_broker(&root), || {
        direct::seam_here(recorded, || {
            broker::connect(
                &Target::Addr(addr),
                addr.port(),
                MUST_HAPPEN,
                ConnectOptions::default(),
            )
        })
    });
    let Err(LanError::Io(error)) = got else {
        panic!("a refused connect is the act's error: {got:?}");
    };
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused,
        "{error}"
    );
    let lines = RECORDED.lock().unwrap().clone();
    assert!(
        lines.iter().any(|line| line.contains("refused")),
        "the server's line went to the path's log: {lines:?}"
    );
}
