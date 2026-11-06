// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node job's reads of its windows' needs (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.14,
//! Q4): the relay header that asks a window for them, the frame each row travels in, and the
//! reads the job runs while it holds the mesh, which hand each change on.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::mpsc;

use aterm_metrics::row::{ManagerEntry, NeedEntry, NeedsRow, Sid, Word};

use super::super::{
    NEEDS_FRAME_MAX, WindowNeeds, WindowNeedsReads, read_needs_frame, write_needs_frame,
};
use super::*;

/// A time the rows below are written at, Unix seconds.
const AT: u64 = 1_793_160_000;

fn sid(n: u8) -> Sid {
    Sid::parse(&format!("s-{n:020x}")).expect("a minted sid")
}

/// A row in which session 1, a worker of session 2, needs `word`.
fn row(at: u64, word: &str) -> NeedsRow {
    NeedsRow {
        at,
        needs: vec![NeedEntry {
            sid: sid(1),
            word: Word::new(word).expect("a need word"),
            harness: false,
            worker_of: Some(sid(2)),
            since: Some(AT - 30),
        }],
        held: Vec::new(),
        managers: vec![ManagerEntry {
            sid: sid(2),
            workers: 1,
        }],
    }
}

/// A sink that sends each map it is handed on the channel it answers.
fn sink() -> (WindowNeeds, mpsc::Receiver<BTreeMap<u32, NeedsRow>>) {
    let (maps, handed) = mpsc::channel();
    let sink: WindowNeeds = Arc::new(move |rows: &BTreeMap<u32, NeedsRow>| {
        let _ = maps.send(rows.clone());
    });
    (sink, handed)
}

/// The next map the sink was handed, within the hang detector.
fn next(handed: &mpsc::Receiver<BTreeMap<u32, NeedsRow>>) -> BTreeMap<u32, NeedsRow> {
    handed
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the sink was handed a map within the hang detector")
}

/// A stand-in window: its relay in `relay_dir`, under this process's pid, takes one NEEDS
/// hand-over until `stop`; once the node job writes `go` it writes each row `rows` hands it as a
/// frame and hangs up when `rows` ends, and says on `ended` when its stream reads its end. The
/// answer is the relay's thread, which ends once `stop` is set, its socket unlinked.
fn stand_in_window(
    relay_dir: &Path,
    stop: &Arc<AtomicBool>,
    rows: mpsc::Receiver<NeedsRow>,
    ended: mpsc::Sender<()>,
) -> std::thread::JoinHandle<()> {
    stand_in_window_as(relay_dir, std::process::id(), stop, rows, ended)
}

/// [`stand_in_window`] under `pid`, which must be a live process's for the node job to dial it.
fn stand_in_window_as(
    relay_dir: &Path,
    pid: u32,
    stop: &Arc<AtomicBool>,
    rows: mpsc::Receiver<NeedsRow>,
    ended: mpsc::Sender<()>,
) -> std::thread::JoinHandle<()> {
    let (dir, stop) = (relay_dir.to_path_buf(), Arc::clone(stop));
    let once = Mutex::new(Some((rows, ended)));
    let relay = std::thread::spawn(move || {
        let _ = serve_window_relay_at(
            &dir,
            pid,
            Some(&stop),
            hosts_every,
            move |header, mut stream| {
                if header != RelayHeader::Needs {
                    return false;
                }
                let Some((rows, ended)) = once.lock().unwrap().take() else {
                    return false;
                };
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
                    let mut go = [0u8; 3];
                    aterm_uds::hangup::Awake(&stream)
                        .read_exact(&mut go)
                        .expect("the node job wrote go");
                    assert_eq!(&go, b"go\n");
                    let _ = stream.set_read_timeout(None);
                    let watch = stream.try_clone().expect("a copy of the stream");
                    std::thread::spawn(move || {
                        let mut byte = [0u8; 1];
                        while let Ok(1) = aterm_uds::hangup::Awake(&watch).read(&mut byte) {}
                        let _ = ended.send(());
                    });
                    for row in rows {
                        write_needs_frame(&mut stream, &row).expect("the window wrote a frame");
                    }
                    aterm_uds::hangup::hang_up(&stream);
                });
                true
            },
        );
    });
    let socket = relay_socket_path(relay_dir, pid);
    aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(5),
        || socket.exists().then_some(()),
    )
    .expect("the stand-in window's relay socket bound");
    relay
}

#[test]
fn the_needs_header_round_trips_and_takes_no_tokens() {
    assert_eq!(RelayHeader::Needs.encode(), "NEEDS\n");
    assert_eq!(
        RelayHeader::parse(&RelayHeader::Needs.encode()),
        Ok(RelayHeader::Needs)
    );
    for bad in [
        "NEEDS x\n",
        "NEEDS peer=127.0.0.1:1\n",
        "NEEDSX\n",
        "needs\n",
    ] {
        assert!(RelayHeader::parse(bad).is_err(), "{bad:?} is refused");
    }
}

#[test]
fn a_needs_frame_round_trips_and_refuses_a_long_length_or_a_trailing_byte() {
    let sent = row(AT, "ask");
    let body = sent.encode();
    let mut wire = Vec::new();
    write_needs_frame(&mut wire, &sent).expect("a frame");
    assert_eq!(
        wire[..4],
        u32::try_from(body.len()).unwrap().to_be_bytes(),
        "the frame opens with its row's length, big-endian"
    );
    assert_eq!(wire[4..], body[..]);
    assert_eq!(
        read_needs_frame(&mut wire.as_slice()).expect("the row"),
        sent
    );

    let kind = |bytes: &[u8]| {
        read_needs_frame(&mut &bytes[..])
            .expect_err("refused")
            .kind()
    };
    // A length past the cap is refused before any row is read (none follows it here); the cap
    // itself is taken, and its read waits for the row, which ends here.
    let past = u32::try_from(NEEDS_FRAME_MAX + 1).unwrap().to_be_bytes();
    assert_eq!(kind(&past), std::io::ErrorKind::InvalidData);
    let cap = u32::try_from(NEEDS_FRAME_MAX).unwrap().to_be_bytes();
    assert_eq!(kind(&cap), std::io::ErrorKind::UnexpectedEof);
    // A row with a byte past it is refused, as the row decodes strictly.
    let mut trailing = u32::try_from(body.len() + 1)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    trailing.extend_from_slice(&body);
    trailing.push(0);
    assert_eq!(kind(&trailing), std::io::ErrorKind::InvalidData);
    // A stream that ends before a frame, or within one, ends the read.
    assert_eq!(kind(&[]), std::io::ErrorKind::UnexpectedEof);
    assert_eq!(
        kind(&wire[..wire.len() - 1]),
        std::io::ErrorKind::UnexpectedEof
    );
}

/// The node job's reads find a window, hand on its row, then its changed row (and nothing for a
/// row that says the same later), then the map without it once it goes: one map per change.
#[test]
fn the_node_hands_on_each_change_of_a_windows_needs_and_its_going() {
    let relay = crate::test_scratch::short_dir("node-needs");
    let stop = Arc::new(AtomicBool::new(false));
    let (rows, window_rows) = mpsc::channel();
    let (ended, _window_ended) = mpsc::channel();
    let window = stand_in_window(relay.path(), &stop, window_rows, ended);
    let (sink, handed) = sink();
    let reads =
        WindowNeedsReads::start(relay.path().to_path_buf(), Duration::from_millis(20), sink)
            .expect("the needs reads start");
    let pid = std::process::id();

    let first = row(AT, "ask");
    rows.send(first.clone()).unwrap();
    assert_eq!(next(&handed), BTreeMap::from([(pid, first.clone())]));

    rows.send(NeedsRow {
        at: AT + 60,
        ..first
    })
    .unwrap();
    let changed = row(AT + 61, "approval");
    rows.send(changed.clone()).unwrap();
    assert_eq!(
        next(&handed),
        BTreeMap::from([(pid, changed)]),
        "the row that said the same later was handed on as no change"
    );

    // The window goes: its relay closes, then it hangs up.
    stop.store(true, Ordering::Release);
    window.join().expect("the stand-in's relay ended");
    drop(rows);
    assert_eq!(next(&handed), BTreeMap::new());

    drop(reads);
    assert!(
        handed.try_recv().is_err(),
        "one map per change, and none as the reads stop"
    );
}

/// The first map the node hands on holds every window its first look found, not the first one
/// read: two windows each write their row, and the sink's first map holds both.
#[test]
fn the_first_map_handed_on_holds_every_window_found() {
    let relay = crate::test_scratch::short_dir("node-needs-two");
    let stop = Arc::new(AtomicBool::new(false));
    let (me, parent) = (std::process::id(), std::os::unix::process::parent_id());
    let (rows_a, window_rows_a) = mpsc::channel();
    let (rows_b, window_rows_b) = mpsc::channel();
    let (ended_a, _a_ended) = mpsc::channel();
    let (ended_b, _b_ended) = mpsc::channel();
    let a = stand_in_window_as(relay.path(), me, &stop, window_rows_a, ended_a);
    let b = stand_in_window_as(relay.path(), parent, &stop, window_rows_b, ended_b);
    let (sink, handed) = sink();
    let reads =
        WindowNeedsReads::start(relay.path().to_path_buf(), Duration::from_millis(20), sink)
            .expect("the needs reads start");
    let (first_a, first_b) = (row(AT, "ask"), row(AT, "stalled"));
    rows_a.send(first_a.clone()).unwrap();
    rows_b.send(first_b.clone()).unwrap();
    assert_eq!(
        next(&handed),
        BTreeMap::from([(me, first_a), (parent, first_b)]),
        "the first map held one window's row"
    );
    drop(reads);
    stop.store(true, Ordering::Release);
    a.join().expect("the first stand-in's relay ended");
    b.join().expect("the second stand-in's relay ended");
    drop((rows_a, rows_b));
}

/// With no window to read, the first map is an empty one: a row of windows gone before the node
/// job started is cleared.
#[test]
fn with_no_window_the_first_map_handed_on_is_empty() {
    let relay = crate::test_scratch::short_dir("node-needs-none");
    let (sink, handed) = sink();
    let reads =
        WindowNeedsReads::start(relay.path().to_path_buf(), Duration::from_millis(20), sink)
            .expect("the needs reads start");
    assert_eq!(next(&handed), BTreeMap::new());
    drop(reads);
    assert!(
        handed.try_recv().is_err(),
        "one map, and none as the reads stop"
    );
}

/// Stopping the reads hangs up on every window it reads and hands nothing more on: the stop is
/// not a window going.
#[test]
fn stopping_the_needs_reads_hangs_up_on_the_windows_and_hands_nothing_on() {
    let relay = crate::test_scratch::short_dir("node-needs-stop");
    let stop = Arc::new(AtomicBool::new(false));
    let (rows, window_rows) = mpsc::channel();
    let (ended, window_ended) = mpsc::channel();
    let window = stand_in_window(relay.path(), &stop, window_rows, ended);
    let (sink, handed) = sink();
    let reads =
        WindowNeedsReads::start(relay.path().to_path_buf(), Duration::from_millis(20), sink)
            .expect("the needs reads start");

    let first = row(AT, "stalled");
    rows.send(first.clone()).unwrap();
    assert_eq!(next(&handed), BTreeMap::from([(std::process::id(), first)]));

    drop(reads);
    window_ended
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the window read the node job's hang-up within the hang detector");
    assert!(
        handed.try_recv().is_err(),
        "a stop hands on no map without the window"
    );

    stop.store(true, Ordering::Release);
    window.join().expect("the stand-in's relay ended");
    drop(rows);
}
