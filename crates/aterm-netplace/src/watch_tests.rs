// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc;
use std::time::Duration;

/// A change of network in the kernel's format: macOS's network-class `KEV_INET_NEW_ADDR`, and on
/// Linux any message of the groups joined.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn address_change() -> Vec<u8> {
    [24_u32, 1, 1, 1, 37, 1]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect()
}

/// A ROUTE CHANGE IS TOLD, A STOP ENDS THE WATCH, A FAILED SOCKET IS LOST ONCE: the reader's
/// route watch tells `changed` at each change the socket names, its drop stops and joins its
/// thread (a hang here is the detector's), and a socket that fails tells `lost` so the reader
/// falls back to its bounded slow look.
/// NEGATIVE CONTROL: a watch whose `RouteWake::Change` arm tells nothing hangs "a route change
/// is told" at the detector; one whose `Failed` arm returns without `lost()` hangs "a failed
/// socket is lost".
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_route_watch_tells_each_change_and_a_failure_once() {
    let hang = Duration::from_secs(60);
    let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    let (told, tells) = mpsc::channel();
    let (gone, losses) = mpsc::channel();
    let watch = super::routes(
        Ok(aterm_uds::network::RouteChanges::over(heard.into())),
        move || {
            let _ = told.send(());
        },
        move || {
            let _ = gone.send(());
        },
    )
    .expect("the watch starts");
    kernel.send(&address_change()).expect("a change sent");
    assert_eq!(tells.recv_timeout(hang), Ok(()), "a route change is told");
    kernel
        .send(&address_change())
        .expect("a second change sent");
    assert_eq!(tells.recv_timeout(hang), Ok(()), "every change is told");
    drop(watch);
    assert!(
        losses.try_recv().is_err(),
        "a stop is no loss of the socket"
    );

    let (heard, peer) = std::os::unix::net::UnixStream::pair().expect("a stream pair");
    let (gone, losses) = mpsc::channel();
    let watch = super::routes(
        Ok(aterm_uds::network::RouteChanges::over(heard.into())),
        || {},
        move || {
            let _ = gone.send(());
        },
    )
    .expect("the watch starts");
    drop(peer);
    assert_eq!(losses.recv_timeout(hang), Ok(()), "a failed socket is lost");
    drop(watch);

    assert!(
        super::routes(Err(std::io::Error::other("refused")), || {}, || {}).is_none(),
        "a socket that did not open is no watch"
    );
}

/// EACH OWNER HEARS THE FILES IT READS AND NO OTHERS: past its first looks (one per file, each
/// of which always tells), the keeper's watch tells for each write of the evidence, never for
/// the record it writes itself (so its own write never wakes it for a look that finds nothing)
/// nor for the home-day ledger no step reads; a reader's watch tells for the record too. The
/// record and the ledger are written first, then the evidence twice, the second once the first
/// was told: a tell for either of the first two is due no later than the first evidence write's,
/// so it comes before the second's. A direct scan's answer (`observations/heard`, a folder down)
/// tells too.
/// NEGATIVE CONTROL: the keeper watching the record too (`Self::Keeper => &FILES`) fails "the
/// keeper's own record write tells it nothing" (`left: ["heard", "heard", "place.toml"]`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn each_owner_hears_the_files_it_reads_and_no_others() {
    let hang = Duration::from_secs(60);
    for reads in [super::Reads::Keeper, super::Reads::Reader] {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = crate::record::dir(root.path());
        crate::record::write(&dir, &crate::record::Written::default()).unwrap();
        let (told, tells) = mpsc::channel();
        let watch = super::place_files(&dir, reads, move |name| {
            let _ = told.send(name);
        })
        .expect("the watch arms");
        let mut first = (0..watch.first_looks())
            .map(|_| tells.recv_timeout(hang).expect("each first look tells"))
            .collect::<Vec<_>>();
        first.sort_unstable();
        let mut expected = super::Reads::files(reads)
            .iter()
            .map(|&(_, name)| name)
            .chain(crate::facts::seam_path().map(|_| super::SCRIPTED))
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(first, expected, "each watched file's first look tells");
        let written = crate::record::Written {
            revision: 1,
            written_ms: 1,
            ..Default::default()
        };
        crate::record::write(&dir, &written).unwrap();
        aterm_uds::private_file::write_private(&dir, crate::heard::DAYS_FILE, b"days\n").unwrap();
        let at = crate::unix_ms_now();
        let source = crate::place::InternetSource::ApiProbe;
        let until_evidence = || {
            let mut seen = Vec::new();
            loop {
                let name = tells.recv_timeout(hang).expect("an evidence write tells");
                seen.push(name);
                if name == crate::heard::FILE {
                    return seen;
                }
            }
        };
        crate::heard::note_internet(&dir, true, source, "fixture", at);
        let mut seen = until_evidence();
        crate::heard::note_internet(&dir, false, source, "fixture", at + 1);
        seen.extend(until_evidence());
        // A direct scan's answer lives a folder down, which a watch of the folder's own entries
        // never heard.
        crate::heard::note_observations(
            &dir,
            vec![crate::place::Answer {
                lane: crate::place::Lane::Fleet,
                member: crate::place::member_key(crate::place::Lane::Fleet, "m1"),
                addr: None,
                via: None,
                iface: None,
                network: None,
                logged: true,
                began_ms: at,
                at_ms: at,
            }],
        );
        let scanned = loop {
            let name = tells
                .recv_timeout(hang)
                .expect("a direct scan's write tells");
            if name == "observations/heard" {
                break name;
            }
            seen.push(name);
        };
        drop(watch);
        seen.sort_unstable();
        let mut want = vec![crate::heard::FILE, crate::heard::FILE];
        if reads == super::Reads::Reader {
            want.push(crate::record::FILE);
        }
        want.sort_unstable();
        assert_eq!(scanned, "observations/heard");
        assert_eq!(
            seen, want,
            "the keeper's own record write tells it nothing, nor the ledger; a reader hears the \
             record ({reads:?})"
        );
    }
}

/// A WALL MOVE IS TOLD, A DROP ENDS THE WATCH: an announced wake or step tells `moved` (the
/// owner steps again at the wall's new now), and the drop stops and joins the thread (a hang
/// here is the detector's). No source, no watch.
/// NEGATIVE CONTROL: a watch that takes the move and tells nothing hangs "a step is told" at the
/// detector.
#[cfg(unix)]
#[test]
fn the_wall_watch_tells_each_move_and_its_drop_joins() {
    use aterm_uds::wall_watch::{Moved, WallWatch};
    let hang = Duration::from_secs(60);
    assert!(super::wall(None, || {}).is_none(), "no source, no watch");
    let source = std::sync::Arc::new(WallWatch::by_hand().expect("doorbell"));
    let (told, tells) = mpsc::channel();
    let watch = super::wall(Some(std::sync::Arc::clone(&source)), move || {
        let _ = told.send(());
    })
    .expect("the watch starts");
    for moved in [
        Moved {
            stepped: true,
            ..Moved::default()
        },
        Moved {
            woke: true,
            ..Moved::default()
        },
    ] {
        source.tell(moved);
        tells.recv_timeout(hang).expect("a step is told");
    }
    drop(watch);
    assert!(
        tells.recv_timeout(Duration::ZERO).is_err(),
        "nothing more was told, and the thread is joined"
    );
}
