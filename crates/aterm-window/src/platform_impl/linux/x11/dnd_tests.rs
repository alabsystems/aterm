// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! The XDND target against a stand-in source on its own connection.

use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use aterm_x11::proto::event::{ClientMessage, Event as XEvent, SelectionNotify, SelectionRequest};
use aterm_x11::proto::xproto::{self, cw, event_mask, PropMode};
use aterm_x11::proto::{ValueList, CURRENT_TIME};
use aterm_x11::Connection;

use super::Window;
use crate::event::{Event, WindowEvent};
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// The hang detector: how long a wait for what must happen may take, which it never needs.
const HANG: Duration = Duration::from_secs(60);

/// What the window reported of the drags.
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Hovered(PathBuf),
    Dropped(PathBuf),
    Cancelled,
}

/// A drag source on its own connection, dragging over `target`.
struct Source {
    conn: Connection,
    window: u32,
    target: u32,
    enter: u32,
    position: u32,
    leave: u32,
    drop: u32,
    finished: u32,
    status: u32,
    incr: u32,
    selection: u32,
    uri_list: u32,
    copy: u32,
    /// What a drag of selected text offers (Firefox, gedit): no `text/uri-list`.
    text: [u32; 3],
}

impl Source {
    /// A conversion on a child of this test's target, not another client's request for the
    /// selection we own. A requestor destroyed before we read its request is no longer ours.
    fn for_target(&self, request: &SelectionRequest) -> bool {
        request.selection == self.selection
            && request.target == self.uri_list
            && self
                .conn
                .send(xproto::query_tree(request.requestor))
                .and_then(|cookie| cookie.reply())
                .is_ok_and(|tree| tree.parent == self.target)
    }

    fn message(&self, type_: u32, values: [u32; 5]) {
        let message = ClientMessage::new32(self.target, type_, values);
        self.conn
            .send_void(xproto::send_event(false, self.target, 0, &message.to_bytes()))
            .unwrap();
        self.conn.flush().unwrap();
    }

    fn enter(&self) {
        self.enter_offering([self.uri_list, 0, 0]);
    }

    /// Version 5, offering `types` (three or fewer, so no `XdndTypeList`).
    fn enter_offering(&self, types: [u32; 3]) {
        self.message(self.enter, [self.window, 5 << 24, types[0], types[1], types[2]]);
    }

    fn position(&self, time: u32) {
        self.message(self.position, [self.window, 0, (10 << 16) | 10, time, self.copy]);
    }

    fn leave(&self) {
        self.message(self.leave, [self.window, 0, 0, 0, 0]);
    }

    fn drop(&self, time: u32) {
        self.message(self.drop, [self.window, 0, time, 0, 0]);
    }

    /// Answer `request` with `file`'s URI, as the owner of `XdndSelection` does.
    fn answer(&self, request: &SelectionRequest, file: &Path) {
        self.answer_list(request, format!("{}\r\n", file_uri(file)).as_bytes());
    }

    /// Answer `request` with `list`, a `text/uri-list` in one property.
    fn answer_list(&self, request: &SelectionRequest, list: &[u8]) {
        self.conn
            .send_void(xproto::change_property(
                PropMode::Replace,
                request.requestor,
                request.property,
                request.target,
                8,
                list,
            ))
            .unwrap();
        self.notify(request, request.property);
    }

    /// Refuse `request` (`SelectionNotify` with no property).
    fn refuse(&self, request: &SelectionRequest) {
        self.notify(request, 0);
    }

    fn notify(&self, request: &SelectionRequest, property: u32) {
        let notify = SelectionNotify {
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property,
        };
        self.conn
            .send_void(xproto::send_event(false, request.requestor, 0, &notify.to_bytes()))
            .unwrap();
        self.conn.flush().unwrap();
    }

    /// The server's time now: the stamp of a property change on the source's window.
    fn now(&self, got: &mut Vec<XEvent>) -> u32 {
        self.conn
            .send_void(xproto::change_property(
                PropMode::Replace,
                self.window,
                self.uri_list,
                self.uri_list,
                8,
                b"",
            ))
            .unwrap();
        self.conn.flush().unwrap();
        let deadline = Instant::now() + HANG;
        loop {
            while let Some(packet) = self.conn.poll_for_event().unwrap() {
                match XEvent::parse(&packet) {
                    Ok(XEvent::PropertyNotify(p)) if p.window == self.window => return p.time,
                    Ok(XEvent::SelectionRequest(r)) if !self.for_target(&r) => self.refuse(&r),
                    Ok(event) => got.push(event),
                    Err(_) => {},
                }
            }
            assert!(Instant::now() < deadline, "the source's PropertyNotify never came");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// `path` as a `file://` URI, escaped as GLib's `g_filename_to_uri` escapes it: every byte but
/// the unreserved ones and `/` (a space is `%20`).
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// A window of the backend and a drag source over it, on `$DISPLAY`. The window holds the
/// shared connection's turn while it lives. Fields drop in order: the window before its loop.
struct Rig {
    _window: Window,
    event_loop: EventLoop<()>,
    source: Source,
    /// What the window reported.
    seen: Vec<Seen>,
    /// What the source received, other clients' selection requests aside.
    got: Vec<XEvent>,
    _turn: MutexGuard<'static, ()>,
}

impl Rig {
    /// `None`, after saying SKIP, where no X server is named.
    fn new() -> Option<Rig> {
        if std::env::var_os("DISPLAY").is_none() {
            eprintln!("SKIP: no DISPLAY");
            return None;
        }
        let turn = super::shared_display_turn();
        let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
        let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
            panic!("an X11 loop");
        };
        let window = Window::new(target, WindowAttributes::default().with_visible(false))
            .expect("an X11 window");

        let (conn, screen) =
            Connection::connect(None).expect("DISPLAY is set: the connection must open");
        let root = conn.setup().roots[screen].root;
        let [enter, position, leave, drop, finished, status, incr, selection, uri_list, copy, text_utf8, utf8_string, text_plain] =
            conn.intern_atoms(&[
                b"XdndEnter",
                b"XdndPosition",
                b"XdndLeave",
                b"XdndDrop",
                b"XdndFinished",
                b"XdndStatus",
                b"INCR",
                b"XdndSelection",
                b"text/uri-list",
                b"XdndActionCopy",
                b"text/plain;charset=utf-8",
                b"UTF8_STRING",
                b"text/plain",
            ])
            .unwrap()[..]
        else {
            unreachable!()
        };
        let source_window = conn.generate_id().unwrap();
        conn.send_void(xproto::create_window(
            0,
            source_window,
            root,
            0,
            0,
            1,
            1,
            0,
            2, // InputOnly
            0,
            &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
        ))
        .unwrap()
        .check()
        .expect("the source's window");
        conn.send_void(xproto::set_selection_owner(source_window, selection, CURRENT_TIME))
            .unwrap();
        let source = Source {
            conn,
            window: source_window,
            target: window.id().0 as u32,
            enter,
            position,
            leave,
            drop,
            finished,
            status,
            incr,
            selection,
            uri_list,
            copy,
            text: [text_utf8, utf8_string, text_plain],
        };
        Some(Rig {
            _window: window,
            event_loop,
            source,
            seen: Vec::new(),
            got: Vec::new(),
            _turn: turn,
        })
    }

    /// The server's time now.
    fn now(&mut self) -> u32 {
        self.source.now(&mut self.got)
    }

    /// Pump the window's event loop and read the source's events until `done` finds what it
    /// waits for in them, failing the test after [`HANG`].
    fn until<R>(&mut self, what: &str, mut done: impl FnMut(&[Seen], &[XEvent]) -> Option<R>) -> R {
        let Rig { event_loop, source, seen, got, .. } = self;
        let app_window = u64::from(source.target);
        let deadline = Instant::now() + HANG;
        loop {
            event_loop.pump_events(Some(Duration::from_millis(5)), |event, _| {
                if let Event::WindowEvent { window_id, event } = event {
                    let file_event = match event {
                        WindowEvent::HoveredFile(path) => Some(Seen::Hovered(path)),
                        WindowEvent::DroppedFile(path) => Some(Seen::Dropped(path)),
                        WindowEvent::HoveredFileCancelled => Some(Seen::Cancelled),
                        _ => None,
                    };
                    if let Some(event) = file_event {
                        assert_eq!(
                            u64::from(window_id),
                            app_window,
                            "files belong to the app window"
                        );
                        seen.push(event);
                    }
                }
            });
            while let Some(packet) = source.conn.poll_for_event().unwrap() {
                match XEvent::parse(&packet) {
                    Ok(XEvent::SelectionRequest(r)) if !source.for_target(&r) => {
                        source.refuse(&r);
                    },
                    Ok(event) => got.push(event),
                    Err(_) => {},
                }
            }
            if let Some(found) = done(seen, got) {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "{what} never came in {HANG:?}; the window saw {seen:?}, the source got {got:?}"
            );
        }
    }

    /// The window's request for the data of the drag whose position came at `time`.
    fn request(&mut self, time: u32) -> SelectionRequest {
        self.until("the drag's request", |_, got| request_at(got, time))
    }

    /// Whether the window accepted the drop, by its `XdndFinished`.
    fn finish(&mut self) -> bool {
        let finished = self.source.finished;
        self.until("the drop's finish", |_, got| finished_with(got, finished))
    }

    /// Begin answering `request` in ICCCM INCR chunks, as GTK answers a list over 256 KiB: an
    /// `INCR` property of `size`, whose deletion asks for the first chunk ([`Rig::chunk`]).
    fn begin_incr(&mut self, request: &SelectionRequest, size: usize) {
        let source = &self.source;
        source
            .conn
            .send_void(xproto::change_window_attributes(
                request.requestor,
                &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
            ))
            .unwrap()
            .check()
            .expect("the source watches the window's properties");
        source
            .conn
            .send_void(xproto::change_property32(
                PropMode::Replace,
                request.requestor,
                request.property,
                source.incr,
                &[u32::try_from(size).unwrap()],
            ))
            .unwrap();
        source.notify(request, request.property);
    }

    /// Wait until the window has taken the property `n` times (the INCR property, then each
    /// chunk), then put `chunk` there.
    fn chunk(&mut self, request: &SelectionRequest, n: usize, chunk: &[u8]) {
        let (window, property) = (request.requestor, request.property);
        self.until("the window's taking of the INCR property or chunk", |_, got| {
            (deletions(got, window, property) >= n).then_some(())
        });
        self.source
            .conn
            .send_void(xproto::change_property(
                PropMode::Replace,
                window,
                property,
                request.target,
                8,
                chunk,
            ))
            .unwrap();
        self.source.conn.flush().unwrap();
    }

    /// Wait until the window has sent `n` refusing `XdndStatus`es.
    fn refusals(&mut self, n: usize, what: &str) {
        let status = self.source.status;
        self.until(what, |_, got| {
            (statuses(got, status).iter().filter(|&&accept| !accept).count() >= n).then_some(())
        });
    }

    /// Forget what the window and the source saw so far.
    fn clear(&mut self) {
        self.seen.clear();
        self.got.clear();
    }
}

/// The window's request for the drag's data made at `time` (`got` holds only the window's).
fn request_at(got: &[XEvent], time: u32) -> Option<SelectionRequest> {
    got.iter().find_map(|event| match event {
        XEvent::SelectionRequest(r) if r.time == time => Some(*r),
        _ => None,
    })
}

/// How many times `window`'s `property` was deleted.
fn deletions(got: &[XEvent], window: u32, property: u32) -> usize {
    got.iter()
        .filter(|event| {
            matches!(event, XEvent::PropertyNotify(p)
                if p.window == window
                    && p.atom == property
                    && p.state == aterm_x11::proto::event::property::DELETE)
        })
        .count()
}

/// The accept bit of each `XdndStatus` the source got, in order.
fn statuses(got: &[XEvent], status: u32) -> Vec<bool> {
    got.iter()
        .filter_map(|event| match event {
            XEvent::ClientMessage(m) if m.type_ == status => Some(m.data32()[1] & 1 == 1),
            _ => None,
        })
        .collect()
}

/// Whether the source got `XdndFinished` with the drop accepted.
fn finished_with(got: &[XEvent], finished: u32) -> Option<bool> {
    got.iter().find_map(|event| match event {
        XEvent::ClientMessage(m) if m.type_ == finished => Some(m.data32()[1] & 1 == 1),
        _ => None,
    })
}

#[test]
fn a_drags_data_is_the_answer_to_its_own_request() {
    let Some(mut rig) = Rig::new() else { return };
    let scratch = aterm_tempfile::tempdir().expect("a scratch dir");
    let file = |name: &str| {
        let path = scratch.path().join(name);
        std::fs::write(&path, b"").expect("a file to drag");
        path
    };
    let (first, second, third) = (file("AAA-first"), file("BBB-second"), file("CCC-third"));

    let t1 = rig.now();
    rig.source.enter();
    rig.source.position(t1);
    let late = rig.request(t1);
    rig.source.leave();
    rig.until("the first drag's end", |seen, _| seen.contains(&Seen::Cancelled).then_some(()));

    std::thread::sleep(Duration::from_millis(2));
    let t2 = rig.now();
    rig.source.enter();
    rig.source.answer(&late, &first);
    rig.source.position(t2);
    let request = rig.request(t2);
    rig.source.answer(&request, &second);
    rig.until("the second drag's hover", |seen, _| {
        seen.contains(&Seen::Hovered(second.clone())).then_some(())
    });
    rig.source.drop(t2);
    assert!(rig.finish(), "the second drop is accepted");
    assert_eq!(
        rig.seen,
        [Seen::Cancelled, Seen::Hovered(second.clone()), Seen::Dropped(second.clone())],
        "the late answer is nobody's: only the second drag's file is hovered and dropped"
    );

    rig.clear();
    std::thread::sleep(Duration::from_millis(2));
    let t3 = rig.now();
    rig.source.enter();
    rig.source.position(t3);
    let request = rig.request(t3);
    rig.source.drop(t3);
    rig.source.answer(&request, &third);
    assert!(rig.finish(), "the third drop is accepted");
    assert_eq!(rig.seen, [Seen::Dropped(third)], "the early drop delivers its file once it comes");

    rig.clear();
    let t4 = rig.now();
    rig.source.enter();
    rig.source.position(t4);
    let old = rig.request(t4);
    rig.begin_incr(&old, 64);
    rig.until("the first INCR header's taking", |_, got| {
        (deletions(got, old.requestor, old.property) >= 1).then_some(())
    });
    rig.source.leave();
    rig.until("the INCR drag's leave", |seen, _| seen.contains(&Seen::Cancelled).then_some(()));

    rig.clear();
    let t5 = rig.now();
    rig.source.enter();
    rig.source.position(t5);
    let new = rig.request(t5);
    rig.begin_incr(&new, 64);
    rig.until("the second INCR header's taking", |_, got| {
        (deletions(got, new.requestor, new.property) >= 1).then_some(())
    });
    rig.source.drop(t5);
    for chunk in [b"file:///stale-from-the-first-incr\r\n".as_slice(), b"".as_slice()] {
        let error = rig
            .source
            .conn
            .send_void(xproto::change_property(
                PropMode::Replace,
                old.requestor,
                old.property,
                old.target,
                8,
                chunk,
            ))
            .unwrap()
            .check()
            .expect_err("the ended requestor is gone");
        assert!(
            matches!(&error, aterm_x11::Error::X11(wire)
            if wire.error_code == 3 && wire.bad_value == old.requestor),
            "{error:?}"
        );
    }
    assert_ne!(old.requestor, new.requestor, "a new conversion never reuses an old identity");
    rig.chunk(&new, 1, b"file:///the-second-incr\r\n");
    rig.chunk(&new, 2, b"");
    let current = PathBuf::from("/the-second-incr");
    assert!(rig.finish(), "the new INCR drop finishes with its own file");
    assert_eq!(rig.seen, [Seen::Dropped(current)]);

    rig.clear();
    let t6 = rig.now();
    let xconn = rig._window.xconn.clone();
    xconn.with_dnd_requestors_refused(|| {
        rig.source.enter();
        rig.source.drop(t6);
        assert!(!rig.finish(), "a synchronous request failure finishes refused");
    });
    assert!(request_at(&rig.got, t6).is_none(), "the refused id sent no conversion");
    assert_eq!(rig.seen, [], "a refused request drops no file");
}

#[test]
fn a_dropped_path_is_the_one_the_source_named() {
    let Some(mut rig) = Rig::new() else { return };
    let scratch = aterm_tempfile::tempdir().expect("a scratch dir");
    let shot = scratch.path().join("Screenshot from 2026-07-23 11-12-52.png");
    std::fs::write(&shot, b"").expect("a file to drag");
    let link = scratch.path().join("Ünïcødé link");
    std::os::unix::fs::symlink(&shot, &link).expect("a symlink to drag");

    let t = rig.now();
    rig.source.enter();
    rig.source.position(t);
    let request = rig.request(t);
    let list = format!("{}\r\n{}\r\n", file_uri(&shot), file_uri(&link));
    assert!(list.contains("Screenshot%20from%202026-07-23%2011-12-52.png"), "{list}");
    rig.source.answer_list(&request, list.as_bytes());
    rig.until("the drag's two hovers", |seen, _| (seen.len() >= 2).then_some(()));
    rig.source.drop(t);
    assert!(rig.finish(), "the drop is accepted");
    assert_eq!(
        rig.seen,
        [
            Seen::Hovered(shot.clone()),
            Seen::Hovered(link.clone()),
            Seen::Dropped(shot),
            Seen::Dropped(link)
        ],
        "each path is the one the source named, spaces kept and the link not resolved"
    );
}

#[test]
fn a_list_drops_its_local_files_and_a_drag_of_none_is_refused() {
    let Some(mut rig) = Rig::new() else { return };
    let scratch = aterm_tempfile::tempdir().expect("a scratch dir");
    let shot = scratch.path().join("Screenshot from 2026-07-23 11-12-52.png");
    std::fs::write(&shot, b"").expect("a file to drag");
    let gone = scratch.path().join("gone.txt");

    let t1 = rig.now();
    rig.source.enter();
    rig.source.position(t1);
    let request = rig.request(t1);
    let list = format!(
        "https://example.com/cat.png\r\n{}\r\ntrash:///old.txt\r\nfile:///tmp/%FF\r\n\
         sftp://host/home/f\r\n{}\r\n",
        file_uri(&shot),
        file_uri(&gone)
    );
    rig.source.answer_list(&request, list.as_bytes());
    rig.until("the drag's two hovers", |seen, _| (seen.len() >= 2).then_some(()));
    rig.source.drop(t1);
    assert!(rig.finish(), "a drop with local files is accepted");
    assert_eq!(
        rig.seen,
        [
            Seen::Hovered(shot.clone()),
            Seen::Hovered(gone.clone()),
            Seen::Dropped(shot),
            Seen::Dropped(gone)
        ],
        "the local files hover and drop; the other entries are skipped"
    );
    assert!(statuses(&rig.got, rig.source.status).iter().all(|&accept| accept));

    rig.clear();
    std::thread::sleep(Duration::from_millis(2));
    let t2 = rig.now();
    rig.source.enter();
    rig.source.position(t2);
    let request = rig.request(t2);
    rig.source.answer_list(
        &request,
        b"https://example.com/cat.png\r\ntrash:///old.txt\r\nsmb://nas/share/f\r\n\
          mtp://phone/DCIM/x.jpg\r\nfile://nas/share/f\r\n",
    );
    rig.refusals(1, "the refusal once the data came");
    rig.source.position(t2);
    rig.refusals(2, "the next position's refusal");
    rig.source.drop(t2);
    assert!(!rig.finish(), "a drop of no local file is refused");
    assert_eq!(rig.seen, [], "nothing hovers or drops");

    rig.clear();
    std::thread::sleep(Duration::from_millis(2));
    let t3 = rig.now();
    rig.source.enter_offering(rig.source.text);
    rig.source.position(t3);
    rig.refusals(1, "the text drag's refusal");
    rig.source.drop(t3);
    assert!(!rig.finish(), "a drop of text is refused");
    assert_eq!(statuses(&rig.got, rig.source.status), [false], "never accepted");
    assert!(request_at(&rig.got, t3).is_none(), "a text drag's data is never asked for");
    assert_eq!(rig.seen, [], "nothing hovers or drops");
}

#[test]
fn a_list_too_large_for_one_property_comes_in_incr_chunks() {
    let Some(mut rig) = Rig::new() else { return };
    let folder = Path::new("/home/someone/Pictures/Screenshots of a long week");
    let paths: Vec<PathBuf> = (0..5000)
        .map(|i| {
            folder.join(format!("Screenshot from 2026-07-23 11-{:02}-{:02}.png", i / 60, i % 60))
        })
        .collect();
    let list: String = paths.iter().map(|path| file_uri(path) + "\r\n").collect();
    assert!(list.len() > 256 * 1024, "{} bytes", list.len());

    let t = rig.now();
    rig.source.enter();
    rig.source.position(t);
    let request = rig.request(t);
    rig.begin_incr(&request, list.len());
    let pieces: Vec<&[u8]> = list.as_bytes().chunks(64 * 1024).collect();
    for (n, piece) in pieces.iter().enumerate() {
        rig.chunk(&request, n + 1, piece);
        if n == 1 {
            rig.source.position(t);
        }
    }
    rig.chunk(&request, pieces.len() + 1, b"");
    rig.until("the drag's hovers", |seen, _| (seen.len() >= paths.len()).then_some(()));
    rig.source.drop(t);
    assert!(rig.finish(), "the drop is accepted");

    let want: Vec<Seen> = paths
        .iter()
        .cloned()
        .map(Seen::Hovered)
        .chain(paths.iter().cloned().map(Seen::Dropped))
        .collect();
    assert_eq!(rig.seen.len(), want.len(), "every path hovers and drops");
    if let Some(at) = rig.seen.iter().zip(&want).position(|(seen, want)| seen != want) {
        panic!("event {at} is {:?}, not {:?}", rig.seen[at], want[at]);
    }
    let requests = rig.got.iter().filter(|e| matches!(e, XEvent::SelectionRequest(_))).count();
    assert_eq!(requests, 1, "the data is asked for once");
    assert!(statuses(&rig.got, rig.source.status).iter().all(|&accept| accept));
}
