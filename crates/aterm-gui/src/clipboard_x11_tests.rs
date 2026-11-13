// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn a_reply_belongs_to_just_one_paste_request() {
    let key = PasteRequest {
        window: 11,
        selection: 12,
        target: 13,
        property: 14,
    };
    let reply = SelectionNotify {
        sequence: 0,
        time: CURRENT_TIME,
        requestor: key.window,
        selection: key.selection,
        target: key.target,
        property: key.property,
    };
    assert!(key.accepts(&reply));
    assert!(key.accepts(&SelectionNotify {
        property: NONE,
        ..reply
    }));
    for foreign in [
        SelectionNotify {
            requestor: 21,
            ..reply
        },
        SelectionNotify {
            selection: 22,
            ..reply
        },
        SelectionNotify {
            target: 23,
            ..reply
        },
        SelectionNotify {
            property: 24,
            ..reply
        },
    ] {
        assert!(!key.accepts(&foreign));
    }
    let mut reads = Reads::default();
    assert!(reads.admit_window(100));
    assert!(!reads.admit_window(100));
    assert!(!reads.admit_window(99));
    assert!(reads.admit_window(101));
    assert!(!reads.admit_window(100));
}

#[test]
fn cross_client_clipboard_round_trip() {
    let clip_name = format!("ATERM_TEST_CLIPBOARD_{}", std::process::id());
    let prim_name = format!("ATERM_TEST_PRIMARY_{}", std::process::id());
    let connect =
        || X11Clipboard::connect_with_selections(clip_name.as_bytes(), Some(prim_name.as_bytes()));
    let (Some(a), Some(b)) = (connect(), connect()) else {
        aterm_spec::host::could_not_run("no X display for the clipboard round-trip");
    };
    let payload = "aterm clipboard ✓ 你好 😀";
    assert!(
        a.set(Sel::Clipboard, payload),
        "a must take CLIPBOARD ownership"
    );
    assert_eq!(
        b.get(Sel::Clipboard).as_deref(),
        Some(payload),
        "b must read back exactly what a copied (cross-client)"
    );
    let prim = "primary selection text";
    assert!(a.set(Sel::Primary, prim));
    assert_eq!(b.get(Sel::Primary).as_deref(), Some(prim));
    assert_eq!(b.get(Sel::Clipboard).as_deref(), Some(payload));
}

struct Owner {
    conn: Connection,
    utf8: Atom,
    incr: Atom,
    stop: Arc<AtomicBool>,
}

struct CancelOwners(Arc<AtomicBool>);

impl Drop for CancelOwners {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn client() -> (Connection, Window) {
    let (conn, screen) = Connection::connect(None).expect("a second connection");
    let root = conn.setup().roots[screen].root;
    let visual = conn.setup().roots[screen].root_visual;
    let win = conn.generate_id().expect("an XID");
    conn.send_void(xproto::create_window(
        0,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        window_class::INPUT_OUTPUT,
        visual,
        &ValueList::new(),
    ))
    .expect("the client's window");
    (conn, win)
}

fn next_event(conn: &Connection) -> Event {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match conn.poll_for_event().expect("the client's connection") {
            Some(packet) => {
                if let Ok(ev) = Event::parse(&packet) {
                    return ev;
                }
            }
            None => {
                assert!(
                    Instant::now() < deadline,
                    "the client heard nothing for a minute"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

impl Owner {
    fn new(name: &str, stop: &Arc<AtomicBool>) -> Owner {
        let (conn, win) = client();
        let [selection, utf8, incr] = conn
            .intern_atoms(&[name.as_bytes(), b"UTF8_STRING", b"INCR"])
            .expect("atoms")[..]
        else {
            unreachable!()
        };
        conn.send_void(xproto::set_selection_owner(win, selection, CURRENT_TIME))
            .expect("SetSelectionOwner");
        let owned = conn
            .send(xproto::get_selection_owner(selection))
            .and_then(|cookie| cookie.reply())
            .expect("GetSelectionOwner");
        assert_eq!(owned.owner, win, "the owner holds the private selection");
        Owner {
            conn,
            utf8,
            incr,
            stop: Arc::clone(stop),
        }
    }

    fn next_event(&self, until: Instant) -> Option<Event> {
        loop {
            if self.stop.load(Ordering::Acquire) {
                return None;
            }
            assert!(Instant::now() < until, "the owner's event did not arrive");
            if let Some(packet) = self.conn.poll_for_event().expect("the owner's connection")
                && let Ok(event) = Event::parse(&packet)
            {
                return Some(event);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn next_request(&self) -> Option<SelectionRequest> {
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            if let Event::SelectionRequest(req) = self.next_event(until)? {
                return Some(req);
            }
        }
    }

    fn watch(&self, window: Window) {
        self.conn
            .send_void(xproto::change_window_attributes(
                window,
                &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
            ))
            .expect("PropertyNotify on the requestor");
    }

    fn wait_for_delete(&self, window: Window, property: Atom) -> bool {
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            let Some(event) = self.next_event(until) else {
                return false;
            };
            if let Event::PropertyNotify(p) = event
                && p.window == window
                && p.atom == property
                && p.state == notify_state::DELETE
            {
                return true;
            }
        }
    }

    fn notify(&self, req: &SelectionRequest, property: Atom) {
        let notify = SelectionNotify {
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property,
        };
        self.conn
            .send_void(xproto::send_event(
                false,
                req.requestor,
                event_mask::NO_EVENT,
                &notify.to_bytes(),
            ))
            .expect("the SelectionNotify");
        self.conn.flush().expect("flush");
    }

    fn answer(&self, req: &SelectionRequest, text: &str) {
        self.conn
            .send_void(xproto::change_property(
                PropMode::Replace,
                req.requestor,
                req.property,
                self.utf8,
                8,
                text.as_bytes(),
            ))
            .expect("the answer");
        self.notify(req, req.property);
    }

    fn finish(&self) {
        self.conn
            .send(xproto::get_input_focus())
            .and_then(|cookie| cookie.reply())
            .expect("the server read the owner's answer");
    }
}

fn serve_incr_slowly(
    owner: &Owner,
    payload: &[u8],
    chunk: usize,
    gap: Duration,
    after_first: impl FnOnce() -> bool,
) -> bool {
    let Some(req) = owner.next_request() else {
        return false;
    };
    assert_eq!(
        req.target, owner.utf8,
        "the reader asks for UTF8_STRING first"
    );
    owner.watch(req.requestor);
    let size = u32::try_from(payload.len()).expect("a 32-bit size");
    owner
        .conn
        .send_void(xproto::change_property32(
            PropMode::Replace,
            req.requestor,
            req.property,
            owner.incr,
            &[size],
        ))
        .expect("the INCR property");
    owner.notify(&req, req.property);
    let mut after_first = Some(after_first);
    for piece in payload.chunks(chunk).chain(std::iter::once(&[][..])) {
        if !owner.wait_for_delete(req.requestor, req.property) {
            return false;
        }
        std::thread::sleep(gap);
        if owner.stop.load(Ordering::Acquire) {
            return false;
        }
        owner
            .conn
            .send_void(xproto::change_property(
                PropMode::Replace,
                req.requestor,
                req.property,
                owner.utf8,
                8,
                piece,
            ))
            .expect("a piece");
        owner.conn.flush().expect("flush");
        if let Some(after_first) = after_first.take()
            && !after_first()
        {
            return false;
        }
    }
    owner.finish();
    true
}

#[test]
fn a_slow_incr_paste_is_read_whole() {
    let clip_name = format!("ATERM_TEST_INCR_CLIPBOARD_{}", std::process::id());
    let prim_name = format!("ATERM_TEST_INCR_PRIMARY_{}", std::process::id());
    let Some(reader) =
        X11Clipboard::connect_with_selections(clip_name.as_bytes(), Some(prim_name.as_bytes()))
    else {
        aterm_spec::host::could_not_run("no X display for the INCR paste");
    };
    let stop = Arc::new(AtomicBool::new(false));
    let owner = Owner::new(&clip_name, &stop);
    let payload: String = (0..300_000).map(|i| format!("{i}\n")).collect();
    std::thread::scope(|scope| {
        let _cancel = CancelOwners(Arc::clone(&stop));
        let owner_thread = scope.spawn(|| {
            serve_incr_slowly(
                &owner,
                payload.as_bytes(),
                64 * 1024,
                Duration::from_millis(40),
                || true,
            )
        });
        let started = Instant::now();
        let got = reader.get(Sel::Clipboard);
        let took = started.elapsed();
        assert!(
            got.as_deref() == Some(payload.as_str()),
            "the paste came back {} of {} bytes after {took:?}",
            got.map_or(0, |t| t.len()),
            payload.len()
        );
        assert!(owner_thread.join().expect("the owner's thread"));
    });
}

#[test]
fn overlapping_pastes_each_get_their_own_answer() {
    let clip_name = format!("ATERM_TEST_OVERLAP_CLIPBOARD_{}", std::process::id());
    let prim_name = format!("ATERM_TEST_OVERLAP_PRIMARY_{}", std::process::id());
    let Some(reader) =
        X11Clipboard::connect_with_selections(clip_name.as_bytes(), Some(prim_name.as_bytes()))
    else {
        aterm_spec::host::could_not_run("no X display for the overlapping pastes");
    };
    let stop = Arc::new(AtomicBool::new(false));
    let clip_owner = Owner::new(&clip_name, &stop);
    let prim_owner = Owner::new(&prim_name, &stop);
    let payload: String = (0..300_000).map(|i| format!("{i}\n")).collect();
    let (streaming_tx, streaming_rx) = channel();
    let (resume_tx, resume_rx) = channel::<()>();
    std::thread::scope(|scope| {
        let _cancel = CancelOwners(Arc::clone(&stop));
        let clip_owner = &clip_owner;
        let payload = &payload;
        let stop = Arc::clone(&stop);
        let clip_owner_thread = scope.spawn(move || {
            serve_incr_slowly(
                clip_owner,
                payload.as_bytes(),
                64 * 1024,
                Duration::ZERO,
                || {
                    streaming_tx.send(()).expect("the test waits");
                    let until = Instant::now() + Duration::from_secs(60);
                    loop {
                        if stop.load(Ordering::Acquire) {
                            return false;
                        }
                        assert!(Instant::now() < until, "the PRIMARY read did not finish");
                        match resume_rx.recv_timeout(Duration::from_millis(20)) {
                            Ok(()) | Err(RecvTimeoutError::Disconnected) => return true,
                            Err(RecvTimeoutError::Timeout) => {}
                        }
                    }
                },
            )
        });
        let prim_owner_thread = scope.spawn(|| {
            let Some(req) = prim_owner.next_request() else {
                return false;
            };
            prim_owner.answer(&req, "PRIMARY-MARK");
            prim_owner.finish();
            true
        });
        let clip_read = scope.spawn(|| reader.get(Sel::Clipboard));
        streaming_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the CLIPBOARD transfer started");
        let primary = reader.get(Sel::Primary);
        drop(resume_tx);
        let clipboard = clip_read.join().expect("the CLIPBOARD read");
        assert!(
            primary.as_deref() == Some("PRIMARY-MARK")
                && clipboard.as_deref() == Some(payload.as_str()),
            "the PRIMARY read during the CLIPBOARD transfer came back {primary:?}, \
             and the CLIPBOARD paste {} of {} bytes",
            clipboard.map_or(0, |t| t.len()),
            payload.len()
        );
        assert!(
            prim_owner_thread
                .join()
                .expect("the PRIMARY owner answered")
        );
        assert!(
            clip_owner_thread
                .join()
                .expect("the CLIPBOARD owner served")
        );
    });
}

#[test]
fn a_late_answer_is_not_taken_by_the_next_paste() {
    let clip_name = format!("ATERM_TEST_LATE_CLIPBOARD_{}", std::process::id());
    let prim_name = format!("ATERM_TEST_LATE_PRIMARY_{}", std::process::id());
    let Some(reader) =
        X11Clipboard::connect_with_selections(clip_name.as_bytes(), Some(prim_name.as_bytes()))
    else {
        aterm_spec::host::could_not_run("no X display for the late answer");
    };
    let stop = Arc::new(AtomicBool::new(false));
    let owner = Owner::new(&clip_name, &stop);
    let (first, second) = std::thread::scope(|scope| {
        let _cancel = CancelOwners(Arc::clone(&stop));
        let owner_thread = scope.spawn(|| {
            let Some(first) = owner.next_request() else {
                return false;
            };
            let Some(second) = owner.next_request() else {
                return false;
            };
            owner.watch(first.requestor);
            owner.answer(&first, "stale");
            if (second.requestor, second.property) == (first.requestor, first.property)
                && !owner.wait_for_delete(first.requestor, first.property)
            {
                return false;
            }
            owner.answer(&second, "fresh");
            owner.finish();
            true
        });
        let first = reader.get_within(Sel::Clipboard, Duration::from_millis(1000));
        let second = reader.get(Sel::Clipboard);
        assert!(owner_thread.join().expect("the owner answered both rounds"));
        (first, second)
    });
    assert_eq!(first, None, "the first read gives up on a silent owner");
    assert_eq!(
        second.as_deref(),
        Some("fresh"),
        "the second read takes its own answer, not the first's"
    );
}

fn answer_type(name: &str) -> Atom {
    let (conn, win) = client();
    let [selection, utf8, property] = conn
        .intern_atoms(&[name.as_bytes(), b"UTF8_STRING", b"ATERM_TEST_ANSWER"])
        .expect("atoms")[..]
    else {
        unreachable!()
    };
    conn.send_void(xproto::convert_selection(
        win,
        selection,
        utf8,
        property,
        CURRENT_TIME,
    ))
    .expect("ConvertSelection");
    conn.flush().expect("flush");
    let answer = loop {
        if let Event::SelectionNotify(n) = next_event(&conn)
            && n.requestor == win
        {
            break n.property;
        }
    };
    if answer == NONE {
        return NONE;
    }
    conn.send(xproto::get_property(false, win, answer, atom::ANY, 0, 0))
        .and_then(|cookie| cookie.reply())
        .expect("the answer's property")
        .type_
}

#[test]
fn a_copy_over_the_request_limit_is_served_by_incr() {
    use std::fmt::Write as _;
    outgoing_values_expire_without_an_x_event();
    let clip_name = format!("ATERM_TEST_SERVE_CLIPBOARD_{}", std::process::id());
    let prim_name = format!("ATERM_TEST_SERVE_PRIMARY_{}", std::process::id());
    let connect =
        || X11Clipboard::connect_with_selections(clip_name.as_bytes(), Some(prim_name.as_bytes()));
    let (Some(owner), Some(reader)) = (connect(), connect()) else {
        aterm_spec::host::could_not_run("no X display for the INCR copy");
    };
    let atoms = &owner.inner.atoms;
    let small = "a small copy\n".repeat(1000);
    assert!(owner.set(Sel::Clipboard, &small), "owner takes CLIPBOARD");
    assert_eq!(
        answer_type(&clip_name),
        atoms.utf8,
        "a small copy goes out in one UTF8_STRING property"
    );
    let limit = owner.inner.conn.maximum_request_bytes();
    let mut big = String::new();
    for line in 0u64.. {
        if big.len() > limit + 1024 * 1024 {
            break;
        }
        let _ = writeln!(big, "{line}");
    }
    assert!(owner.set(Sel::Clipboard, &big), "owner takes CLIPBOARD");
    let big_type = answer_type(&clip_name);
    let got = reader.get(Sel::Clipboard);
    assert!(
        big_type == atoms.incr && got.as_deref() == Some(big.as_str()),
        "a {}-byte copy over the {limit}-byte request limit went out as type {big_type} \
         (INCR is {}), and another client read {} bytes of it",
        big.len(),
        atoms.incr,
        got.map_or(0, |t| t.len()),
    );
}

fn outgoing_values_expire_without_an_x_event() {
    use std::sync::atomic::AtomicUsize;

    const HANG: Duration = Duration::from_secs(60);
    let base = Instant::now();
    let mut refused = OutgoingSends::default();
    assert!(
        refused
            .start_with((1, 2), 3, b"not admitted".to_vec(), base, |_| {
                Err(std::io::Error::other("injected worker refusal"))
            })
            .is_none()
    );
    {
        let state = refused.shared.state.lock().unwrap();
        assert!(!state.running && state.sends.is_empty());
    }
    assert!(refused.worker.is_none());

    let clock = Arc::new(Mutex::new(base));
    let starts = Arc::new(AtomicUsize::new(0));
    let (parked_tx, parked_rx) = channel();
    let (ended_tx, ended_rx) = channel();
    let spawn = || {
        let clock = Arc::clone(&clock);
        let starts = Arc::clone(&starts);
        let parked = parked_tx.clone();
        let ended = ended_tx.clone();
        move |shared| {
            starts.fetch_add(1, Ordering::AcqRel);
            std::thread::Builder::new()
                .name("aterm-test-incr-expiry".into())
                .spawn(move || {
                    expire_outgoing(
                        shared,
                        || *clock.lock().unwrap(),
                        |due| parked.send(due).expect("the test awaits the timer's park"),
                    );
                    ended.send(()).expect("the test awaits the timer's end");
                })
        }
    };
    let wait_for_due = |due| {
        let until = Instant::now() + HANG;
        loop {
            assert!(Instant::now() < until, "the expiry timer did not arm");
            let observed = parked_rx
                .recv_timeout(until.saturating_duration_since(Instant::now()))
                .expect("the expiry timer did not arm");
            if observed == due {
                break;
            }
        }
    };
    let advance = |shared: &OutgoingShared, now| {
        let _state = shared.state.lock().unwrap();
        *clock.lock().unwrap() = now;
        shared.changed.notify_one();
    };
    let mut sends = OutgoingSends::default();
    let old = (11, 14);
    let new = (21, 24);
    let first_id = sends
        .start_with(old, 3, b"old-transfer".to_vec(), base, spawn())
        .unwrap();
    wait_for_due(base + SERVE_STALL);
    let first = sends.chunk(old, 4, base).unwrap();
    assert_eq!(first.bytes, b"old-");
    let progressed = base + Duration::from_secs(5);
    advance(&sends.shared, progressed);
    sends.sent(&first, true, progressed);
    wait_for_due(progressed + SERVE_STALL);

    let copied = base + Duration::from_secs(10);
    advance(&sends.shared, copied);
    sends
        .start_with(new, 3, b"new-copy".to_vec(), copied, spawn())
        .unwrap();
    let pending = sends.chunk(old, 4, copied).unwrap();
    assert_eq!(pending.bytes, b"tran");
    assert_eq!(sends.chunk(new, 4, copied).unwrap().bytes, b"new-");
    assert_eq!(starts.load(Ordering::Acquire), 1);
    let old_due = progressed + SERVE_STALL;
    let before = old_due - Duration::from_nanos(1);
    assert!(sends.chunk(old, 4, before).is_some());
    advance(&sends.shared, old_due);
    wait_for_due(copied + SERVE_STALL);
    assert!(sends.chunk(old, 4, old_due).is_none());
    assert!(sends.chunk(new, 4, old_due).is_some());
    sends.sent(&pending, true, old_due);
    assert!(sends.chunk(old, 4, old_due).is_none(), "no late revival");

    let replacement = sends
        .start_with(old, 3, b"replacement".to_vec(), old_due, spawn())
        .unwrap();
    assert_ne!(first_id, replacement);
    sends.sent(&pending, true, old_due);
    let replacement_chunk = sends.chunk(old, 4, old_due).unwrap();
    assert_eq!(
        replacement_chunk.bytes, b"repl",
        "old completion is not new progress"
    );
    let failing = sends.chunk(new, 4, old_due).unwrap();
    sends.sent(&failing, false, old_due);
    assert!(sends.chunk(new, 4, old_due).is_none());
    let final_due = old_due + SERVE_STALL;
    advance(&sends.shared, final_due);
    ended_rx
        .recv_timeout(HANG)
        .expect("the idle table did not end its timer");
    {
        let state = sends.shared.state.lock().unwrap();
        assert!(state.sends.is_empty() && !state.running);
    }

    sends
        .start_with(old, 3, b"abc".to_vec(), final_due, spawn())
        .unwrap();
    wait_for_due(final_due + SERVE_STALL);
    assert_eq!(starts.load(Ordering::Acquire), 2);
    let body = sends.chunk(old, 4, final_due).unwrap();
    sends.sent(&body, true, final_due);
    let end = sends.chunk(old, 4, final_due).unwrap();
    assert!(end.bytes.is_empty());
    sends.sent(&end, true, final_due);
    ended_rx
        .recv_timeout(HANG)
        .expect("the completed table did not end its timer");
    drop(sends);
    for _ in parked_rx.try_iter() {}

    struct CloseOnDrop(Arc<OutgoingShared>);
    impl Drop for CloseOnDrop {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let mut unwinding = OutgoingSends::default();
    unwinding
        .start_with(old, 3, b"unwind".to_vec(), final_due, spawn())
        .unwrap();
    wait_for_due(final_due + SERVE_STALL);
    let shared = Arc::clone(&unwinding.shared);
    let (dropped_tx, dropped_rx) = channel();
    std::thread::scope(|scope| {
        let _close = CloseOnDrop(Arc::clone(&shared));
        scope.spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _owner = unwinding;
                panic!("exercise serving-loop unwind");
            }));
            dropped_tx.send(caught.is_err()).unwrap();
        });
        assert_eq!(dropped_rx.recv_timeout(HANG), Ok(true));
        let state = shared.state.lock().unwrap();
        assert!(state.closed && !state.running && state.sends.is_empty());
    });
    ended_rx
        .recv_timeout(HANG)
        .expect("the unwound timer did not end");

    struct EndOnDrop(Sender<()>);
    impl Drop for EndOnDrop {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let mut panicked = OutgoingSends::default();
    let (failed_tx, failed_rx) = channel();
    let failed_id = panicked
        .start_with(old, 3, b"worker panic".to_vec(), final_due, |shared| {
            std::thread::Builder::new()
                .name("aterm-test-incr-panic".into())
                .spawn(move || {
                    let _ended = EndOnDrop(failed_tx);
                    expire_outgoing(
                        shared,
                        || final_due,
                        |_| panic!("exercise expiry-worker unwind"),
                    );
                })
        })
        .unwrap();
    failed_rx
        .recv_timeout(HANG)
        .expect("the panicked expiry worker did not clean up");
    assert!(panicked.shared.state.is_poisoned());
    {
        let state = panicked
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        assert!(!state.running && state.sends.is_empty());
    }
    for _ in parked_rx.try_iter() {}
    let recovered_id = panicked
        .start_with(old, 3, b"recovered".to_vec(), final_due, spawn())
        .unwrap();
    assert_ne!(failed_id, recovered_id);
    wait_for_due(final_due + SERVE_STALL);
    let recovered = panicked.chunk(old, 4, final_due).unwrap();
    assert_eq!(recovered.bytes, b"reco");
    panicked.sent(&recovered, false, final_due);
    ended_rx
        .recv_timeout(HANG)
        .expect("the recovered expiry worker did not end");
    drop(panicked);
}
