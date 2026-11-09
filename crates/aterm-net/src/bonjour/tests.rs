// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The browse against a stand-in broker served on this thread's seam socket: what the broker
//! streams reaches the callback, a callback or `going` ends it, and a missing broker or one that
//! fails the browse is an error.

use super::*;
use aterm_uds::lan::wire::{self, Frame, Reader, Request};
use aterm_uds::lan::{Seam, seam_here};
use std::time::Instant;

/// A hang detector, never a latency budget.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

#[cfg(target_os = "macos")]
fn instance(name: &str, interface: u32) -> Instance {
    Instance {
        name: name.to_owned(),
        service: "_alab._tcp".to_owned(),
        domain: "local.".to_owned(),
        interface,
    }
}

/// A stand-in broker at a fresh socket that answers one browse with `frames`, then waits for the
/// client to close; the seam pointing this thread's client at it, and the request it read
/// (macOS: elsewhere the client browses nothing).
#[cfg(target_os = "macos")]
fn stand_in(
    frames: Vec<Frame>,
) -> (
    Seam,
    aterm_tempfile::TempDir,
    std::thread::JoinHandle<Option<Request>>,
) {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("broker.sock");
    let listener = aterm_uds::CtlListener::bind(&path).unwrap();
    let served = std::thread::spawn(move || {
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        wire::send(
            &stream,
            &Frame::Hello {
                protocol: aterm_uds::lan::PROTOCOL,
                build: "stand-in".into(),
            },
        )
        .unwrap();
        let mut reader = Reader::new(0);
        let deadline = Some(Instant::now() + MUST_HAPPEN);
        assert!(matches!(
            reader.frame(&stream, deadline).unwrap(),
            Some(Frame::Hello { .. })
        ));
        let request = match reader.frame(&stream, deadline) {
            Ok(Some(Frame::Request(request))) => Some(request),
            _ => None,
        };
        for frame in &frames {
            wire::send(&stream, frame).unwrap();
        }
        // The client's close ends the browse, as it ends the broker's.
        let _ = reader.frame(&stream, deadline);
        request
    });
    let seam = Seam {
        socket: Some(path),
        ..Seam::default()
    };
    (seam, root, served)
}

#[cfg(target_os = "macos")]
#[test]
fn each_instance_reaches_the_callback_until_it_breaks() {
    let (seam, _root, served) = stand_in(vec![
        Frame::Event(Heard::Added(instance("one", 4))),
        Frame::Event(Heard::Removed(instance("one", 4))),
        Frame::Event(Heard::Added(instance("two", 7))),
        Frame::Event(Heard::Added(instance("unread", 7))),
    ]);
    let mut heard = Vec::new();
    seam_here(seam, || {
        browse(c"_alab._tcp", c"local.", |event| {
            heard.push((event.added, event.interface, event.name.to_owned()));
            if event.name == c"two" {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
    })
    .unwrap();
    assert_eq!(
        heard,
        [
            (true, 4, c"one".to_owned()),
            (false, 4, c"one".to_owned()),
            (true, 7, c"two".to_owned()),
        ]
    );
    assert_eq!(
        served.join().unwrap(),
        Some(Request::Browse {
            service: "_alab._tcp".into(),
            domain: "local.".into(),
        })
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_silent_browse_ends_once_going_says_no() {
    let (seam, _root, served) = stand_in(Vec::new());
    let mut looks = 0;
    seam_here(seam, || {
        browse_while(
            c"_alab._tcp",
            c"local.",
            Duration::from_millis(1),
            || {
                looks += 1;
                looks < 3
            },
            |_| panic!("the stand-in streams nothing"),
        )
    })
    .unwrap();
    assert_eq!(looks, 3);
    assert!(served.join().unwrap().is_some());
}

#[cfg(target_os = "macos")]
#[test]
fn a_browse_the_broker_fails_is_an_error() {
    let (seam, _root, served) = stand_in(vec![Frame::Error {
        code: -65563,
        words: "the daemon is not running".into(),
    }]);
    seam_here(seam, || {
        browse(c"_alab._tcp", c"local.", |_| ControlFlow::Continue(()))
    })
    .unwrap_err();
    served.join().unwrap();
}

#[test]
fn no_broker_is_not_connected() {
    let root = aterm_tempfile::tempdir().unwrap();
    let seam = Seam {
        socket: Some(root.path().join("absent.sock")),
        ..Seam::default()
    };
    let error = seam_here(seam, || {
        browse(c"_alab._tcp", c"local.", |_| ControlFlow::Continue(()))
    })
    .unwrap_err();
    if cfg!(target_os = "macos") {
        assert_eq!(error.kind(), io::ErrorKind::NotConnected, "{error}");
    } else {
        assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{error}");
    }
}
