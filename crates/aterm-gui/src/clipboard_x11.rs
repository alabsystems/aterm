// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Self-contained X11 clipboard (`CLIPBOARD` + `PRIMARY`) over `aterm_x11`.

use std::collections::HashMap;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_x11::Connection;
use aterm_x11::proto::event::{Event, SelectionNotify, SelectionRequest, property as notify_state};
use aterm_x11::proto::xproto::{self, GetPropertyReply, PropMode, cw, event_mask, window_class};
use aterm_x11::proto::{Atom, CURRENT_TIME, NONE, ValueList, Visualid, Window, atom};

/// Which X selection to read/write.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sel {
    Clipboard,
    Primary,
}

enum PasteOutcome {
    Text(String),
    Unsupported,
    Timeout,
}

/// How long a paste waits on an owner whose answer stopped moving ([`X11Clipboard::get`]). A unit
/// test's owner is its own thread or its own clipboard, which answers, so there it is the hang
/// detector; the test of a silent owner passes its own bound ([`X11Clipboard::get_within`]).
#[cfg(not(test))]
const PASTE_STALL: Duration = Duration::from_millis(1000);
#[cfg(test)]
const PASTE_STALL: Duration = aterm_spec::wait::MUST_HAPPEN;

const MAX_PASTE_BYTES: usize = 64 * 1024 * 1024;

const INCR_CHUNK: usize = 256 * 1024;

/// How long an INCR transfer waits for its requestor to take the next chunk. A unit test's
/// requestor is its own clipboard, which takes each one, so there it is the hang detector.
#[cfg(not(test))]
const SERVE_STALL: Duration = Duration::from_secs(30);
#[cfg(test)]
const SERVE_STALL: Duration = aterm_spec::wait::MUST_HAPPEN;

struct Atoms {
    clipboard: Atom,
    primary: Atom,
    utf8: Atom,
    /// The polymorphic ICCCM `TEXT` target: on serve we answer it as UTF-8 (labelling
    /// the property `UTF8_STRING`); on paste we request it as a fallback so a legacy
    /// owner that exports only `TEXT`/`STRING` is still readable.
    text: Atom,
    targets: Atom,
    text_plain: Atom,
    /// The property, on a read's own requestor window, that its `ConvertSelection`
    /// asks the owner to put the answer in.
    recv: Atom,
    /// The `INCR` atom: an owner answering a large paste with a property of
    /// this type streams the data in chunks via `PropertyNotify` (ICCCM INCR).
    incr: Atom,
}

struct Read {
    key: PasteRequest,
    /// The map entry owns this round's server window until it ends.
    _requestor: Requestor,
    /// When the read last moved: its request, the owner's answer, each INCR
    /// chunk.
    moved: Instant,
    /// Set once the owner answered INCR: the transfer so far.
    incr: Option<Incr>,
    tx: Sender<PasteOutcome>,
}

struct Incr {
    property: Atom,
    type_: Atom,
    bytes: Vec<u8>,
}

struct Outgoing {
    id: u64,
    /// The chunks' type: the answer's own (`UTF8_STRING`, `STRING`, ...).
    type_: Atom,
    bytes: Vec<u8>,
    /// How much of `bytes` has gone out.
    sent: usize,
    /// When the requestor last took a chunk.
    moved: Instant,
}

/// The serving thread alone starts and advances transfers. The timer only
/// removes expired ones; it never touches XCB or the current clipboard text.
#[derive(Default)]
struct OutgoingState {
    sends: HashMap<(Window, Atom), Outgoing>,
    next_id: u64,
    running: bool,
    closed: bool,
}

impl OutgoingState {
    fn expire(&mut self, now: Instant) {
        self.sends
            .retain(|_, out| now.saturating_duration_since(out.moved) < SERVE_STALL);
    }

    fn next_due(&self) -> Option<Instant> {
        self.sends
            .values()
            .filter_map(|out| out.moved.checked_add(SERVE_STALL))
            .min()
    }
}

#[derive(Default)]
struct OutgoingShared {
    state: Mutex<OutgoingState>,
    changed: Condvar,
}

impl OutgoingShared {
    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.closed = true;
        state.sends.clear();
        self.changed.notify_one();
    }
}

struct OutgoingChunk {
    key: (Window, Atom),
    id: u64,
    type_: Atom,
    bytes: Vec<u8>,
    end: usize,
}

/// Outgoing values and their demand-only timer. There is at most one worker,
/// joined before its replacement, and none remains once the table is empty.
#[derive(Default)]
struct OutgoingSends {
    shared: Arc<OutgoingShared>,
    worker: Option<JoinHandle<()>>,
}

impl OutgoingSends {
    fn start(
        &mut self,
        key: (Window, Atom),
        type_: Atom,
        bytes: Vec<u8>,
        now: Instant,
    ) -> Option<u64> {
        self.start_with(key, type_, bytes, now, |shared| {
            std::thread::Builder::new()
                .name("aterm-x11-incr-expiry".into())
                .spawn(move || expire_outgoing(shared, Instant::now, |_| {}))
        })
    }

    fn start_with(
        &mut self,
        key: (Window, Atom),
        type_: Atom,
        bytes: Vec<u8>,
        now: Instant,
        spawn: impl FnOnce(Arc<OutgoingShared>) -> std::io::Result<JoinHandle<()>>,
    ) -> Option<u64> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return None;
        }
        let id = state.next_id.checked_add(1)?;
        if !state.running {
            drop(state);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if state.closed {
                return None;
            }
            state.running = true;
            match spawn(Arc::clone(&self.shared)) {
                Ok(worker) => self.worker = Some(worker),
                Err(_) => {
                    state.running = false;
                    return None;
                }
            }
        }
        state.next_id = id;
        state.sends.insert(
            key,
            Outgoing {
                id,
                type_,
                bytes,
                sent: 0,
                moved: now,
            },
        );
        self.shared.changed.notify_one();
        Some(id)
    }

    fn remove(&self, key: (Window, Atom)) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.sends.remove(&key);
        self.shared.changed.notify_one();
    }

    fn chunk(&self, key: (Window, Atom), limit: usize, now: Instant) -> Option<OutgoingChunk> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.expire(now);
        self.shared.changed.notify_one();
        let out = state.sends.get(&key)?;
        let end = out.bytes.len().min(out.sent.saturating_add(limit));
        Some(OutgoingChunk {
            key,
            id: out.id,
            type_: out.type_,
            bytes: out.bytes[out.sent..end].to_vec(),
            end,
        })
    }

    fn sent(&self, chunk: &OutgoingChunk, sent: bool, now: Instant) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.expire(now);
        if let Some(out) = state.sends.get_mut(&chunk.key)
            && out.id == chunk.id
        {
            if !sent || chunk.bytes.is_empty() {
                state.sends.remove(&chunk.key);
            } else {
                out.sent = chunk.end;
                out.moved = now;
            }
        }
        self.shared.changed.notify_one();
    }
}

impl Drop for OutgoingSends {
    fn drop(&mut self) {
        self.shared.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct OutgoingTimerExit {
    shared: Arc<OutgoingShared>,
    armed: bool,
}

impl Drop for OutgoingTimerExit {
    fn drop(&mut self) {
        if self.armed {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.sends.clear();
            state.running = false;
        }
    }
}

fn expire_outgoing(
    shared: Arc<OutgoingShared>,
    now: impl Fn() -> Instant,
    parking: impl Fn(Instant),
) {
    let mut exit = OutgoingTimerExit {
        shared: Arc::clone(&shared),
        armed: true,
    };
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        let observed = now();
        state.expire(observed);
        let due = (!state.closed).then(|| state.next_due()).flatten();
        let Some(due) = due else {
            state.sends.clear();
            state.running = false;
            exit.armed = false;
            return;
        };
        parking(due);
        (state, _) = shared
            .changed
            .wait_timeout(state, due.saturating_duration_since(now()))
            .unwrap_or_else(PoisonError::into_inner);
    }
}

struct PasteRequest {
    window: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
}

impl PasteRequest {
    fn accepts(&self, reply: &SelectionNotify) -> bool {
        reply.requestor == self.window
            && reply.selection == self.selection
            && reply.target == self.target
            && (reply.property == NONE || reply.property == self.property)
    }
}

struct Requestor {
    conn: Arc<Connection>,
    window: Window,
}

impl Drop for Requestor {
    fn drop(&mut self) {
        let _ = self.conn.send_void(xproto::destroy_window(self.window));
        let _ = self.conn.flush();
    }
}

/// The foreign-owner reads in flight, by their never-reused requestor window.
#[derive(Default)]
struct Reads {
    active: HashMap<Window, Read>,
    /// libxcb can ask XC-MISC for reusable ids when its range is exhausted.
    last_window: Window,
}

impl Reads {
    fn admit_window(&mut self, window: Window) -> bool {
        if window <= self.last_window {
            return false;
        }
        self.last_window = window;
        true
    }

    fn end(&mut self, win: Window, outcome: PasteOutcome) {
        if let Some(read) = self.active.remove(&win) {
            let _ = read.tx.send(outcome);
        }
    }
}

struct Inner {
    conn: Arc<Connection>,
    /// The window our selections are owned by.
    win: Window,
    /// The root window and visual requestor windows are made with.
    root: Window,
    visual: Visualid,
    atoms: Atoms,
    /// Text we currently serve for CLIPBOARD (the selection we own), or `None`.
    clipboard: Mutex<Option<String>>,
    /// Text we currently serve for PRIMARY, or `None`.
    primary: Mutex<Option<String>>,
    /// The foreign-owner reads in flight; the event thread fulfils them.
    reads: Mutex<Reads>,
}

impl Inner {
    fn slot(&self, sel: Atom) -> Option<&Mutex<Option<String>>> {
        if sel == self.atoms.clipboard {
            Some(&self.clipboard)
        } else if sel == self.atoms.primary {
            Some(&self.primary)
        } else {
            None
        }
    }

    fn requestor(&self, reads: &mut Reads) -> Option<Requestor> {
        let win = self.conn.generate_id().ok()?;
        if !reads.admit_window(win) {
            return None;
        }
        let requestor = Requestor {
            conn: Arc::clone(&self.conn),
            window: win,
        };
        drop(
            self.conn
                .send_void(xproto::create_window(
                    0,
                    win,
                    self.root,
                    0,
                    0,
                    1,
                    1,
                    0,
                    window_class::INPUT_OUTPUT,
                    self.visual,
                    &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
                ))
                .ok()?,
        );
        Some(requestor)
    }

    fn incr_chunk(&self) -> usize {
        INCR_CHUNK
            .min(self.conn.maximum_request_bytes().saturating_sub(64))
            .max(4096)
    }

    fn take_property(&self, win: Window, property: Atom) -> Option<GetPropertyReply> {
        self.conn
            .send(xproto::get_property(
                true,
                win,
                property,
                atom::ANY,
                0,
                u32::MAX,
            ))
            .ok()?
            .reply()
            .ok()
    }
}

struct PendingRead<'a> {
    inner: &'a Inner,
    window: Window,
}

impl Drop for PendingRead<'_> {
    fn drop(&mut self) {
        let retired = self
            .inner
            .reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .remove(&self.window);
        drop(retired);
    }
}

/// A live X11 clipboard.
pub(crate) struct X11Clipboard {
    inner: std::sync::Arc<Inner>,
}

impl X11Clipboard {
    /// The process-wide clipboard, connected lazily.
    pub(crate) fn get_handle() -> Option<&'static X11Clipboard> {
        static INSTANCE: OnceLock<Option<X11Clipboard>> = OnceLock::new();
        INSTANCE.get_or_init(X11Clipboard::connect).as_ref()
    }

    fn connect() -> Option<X11Clipboard> {
        Self::connect_with_selections(b"CLIPBOARD", None)
    }

    fn connect_with_selections(
        clipboard_name: &[u8],
        primary_override: Option<&[u8]>,
    ) -> Option<X11Clipboard> {
        let (conn, screen_num) = Connection::connect(None).ok()?;
        let screen = conn.setup().roots.get(screen_num)?;
        let root = screen.root;
        let root_visual = screen.root_visual;
        let win = conn.generate_id().ok()?;
        drop(
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
                root_visual,
                &ValueList::new(),
            ))
            .ok()?,
        );
        let [clipboard, utf8, text, targets, text_plain, recv, incr] = conn
            .intern_atoms(&[
                clipboard_name,
                b"UTF8_STRING",
                b"TEXT",
                b"TARGETS",
                b"text/plain;charset=utf-8",
                b"ATERM_CLIPBOARD_RECV",
                b"INCR",
            ])
            .ok()?[..]
        else {
            return None;
        };
        let primary = match primary_override {
            Some(name) => *conn.intern_atoms(&[name]).ok()?.first()?,
            None => atom::PRIMARY,
        };
        let atoms = Atoms {
            clipboard,
            primary,
            utf8,
            text,
            targets,
            text_plain,
            recv,
            incr,
        };
        conn.flush().ok()?;
        let inner = std::sync::Arc::new(Inner {
            conn: Arc::new(conn),
            win,
            root,
            visual: root_visual,
            atoms,
            clipboard: Mutex::new(None),
            primary: Mutex::new(None),
            reads: Mutex::new(Reads::default()),
        });
        let thread_inner = std::sync::Arc::clone(&inner);
        std::thread::Builder::new()
            .name("aterm-x11-clipboard".into())
            .spawn(move || serve(&thread_inner))
            .ok()?;
        Some(X11Clipboard { inner })
    }

    /// Take ownership of `sel` and serve `text` to any client that pastes it.
    pub(crate) fn set(&self, sel: Sel, text: &str) -> bool {
        let atom = self.sel_atom(sel);
        if let Some(slot) = self.inner.slot(atom) {
            *slot.lock().unwrap() = Some(text.to_owned());
        }
        let conn = &self.inner.conn;
        conn.send_void(xproto::set_selection_owner(
            self.inner.win,
            atom,
            CURRENT_TIME,
        ))
        .is_ok()
            && conn
                .send(xproto::get_selection_owner(atom))
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some_and(|reply| reply.owner == self.inner.win)
    }

    /// Read the current contents of `sel` as text, or `None` if empty / unavailable.
    pub(crate) fn get(&self, sel: Sel) -> Option<String> {
        self.get_within(sel, PASTE_STALL)
    }

    /// [`get`](Self::get), giving up on an owner once the read has not moved for `stall`.
    fn get_within(&self, sel: Sel, stall: Duration) -> Option<String> {
        let atom = self.sel_atom(sel);
        if let Some(t) = self.get_owned(sel) {
            return Some(t);
        }
        for target in [self.inner.atoms.utf8, self.inner.atoms.text, atom::STRING] {
            match self.convert_once(atom, target, stall) {
                PasteOutcome::Text(t) => return Some(t),
                PasteOutcome::Unsupported => continue,
                PasteOutcome::Timeout => return None,
            }
        }
        None
    }

    /// The non-blocking fast path of [`get`](Self::get).
    pub(crate) fn get_owned(&self, sel: Sel) -> Option<String> {
        let atom = self.sel_atom(sel);
        self.inner
            .slot(atom)
            .and_then(|slot| slot.lock().unwrap().clone())
    }

    fn convert_once(&self, selection: Atom, target: Atom, stall: Duration) -> PasteOutcome {
        let inner = &*self.inner;
        let (tx, rx) = channel();
        let win = {
            let mut reads = inner.reads.lock().unwrap();
            let Some(requestor) = inner.requestor(&mut reads) else {
                return PasteOutcome::Timeout;
            };
            let win = requestor.window;
            reads.active.insert(
                win,
                Read {
                    key: PasteRequest {
                        window: win,
                        selection,
                        target,
                        property: inner.atoms.recv,
                    },
                    _requestor: requestor,
                    moved: Instant::now(),
                    incr: None,
                    tx,
                },
            );
            win
        };
        let _pending = PendingRead { inner, window: win };
        let req = inner.conn.send_void(xproto::convert_selection(
            win,
            selection,
            target,
            inner.atoms.recv,
            CURRENT_TIME,
        ));
        if req.is_err() || inner.conn.flush().is_err() {
            return PasteOutcome::Timeout;
        }
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(outcome) => return outcome,
                Err(RecvTimeoutError::Disconnected) => return PasteOutcome::Timeout,
                Err(RecvTimeoutError::Timeout) => {
                    let reads = inner.reads.lock().unwrap();
                    match reads.active.get(&win) {
                        None => return rx.try_recv().unwrap_or(PasteOutcome::Timeout),
                        Some(read) if read.moved.elapsed() >= stall => {
                            return PasteOutcome::Timeout;
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }

    fn sel_atom(&self, sel: Sel) -> Atom {
        match sel {
            Sel::Clipboard => self.inner.atoms.clipboard,
            Sel::Primary => self.inner.atoms.primary,
        }
    }
}

fn serve(inner: &Inner) {
    let mut sends = OutgoingSends::default();
    loop {
        let ev = match inner.conn.wait_for_event() {
            Ok(packet) => match Event::parse(&packet) {
                Ok(ev) => ev,
                Err(_) => continue,
            },
            Err(_) => return,
        };
        match ev {
            Event::SelectionRequest(req) => serve_request(inner, &mut sends, &req),
            Event::SelectionClear(clear) => {
                // Another client took the selection; stop serving our stale copy.
                if let Some(slot) = inner.slot(clear.selection) {
                    *slot.lock().unwrap() = None;
                }
            }
            Event::SelectionNotify(ev) => take_answer(inner, &ev),
            Event::PropertyNotify(p) if p.state == notify_state::NEW_VALUE => {
                take_chunk(inner, p.window, p.atom);
            }
            Event::PropertyNotify(p) if p.state == notify_state::DELETE => {
                send_chunk(inner, &mut sends, p.window, p.atom);
            }
            _ => {}
        }
    }
}

fn serve_request(inner: &Inner, sends: &mut OutgoingSends, req: &SelectionRequest) {
    let a = &inner.atoms;
    let property = if req.property == NONE {
        req.target
    } else {
        req.property
    };
    sends.remove((req.requestor, property));
    let text = inner
        .slot(req.selection)
        .and_then(|slot| slot.lock().unwrap().clone());
    let ok = match text {
        None => false,
        Some(_) if req.target == a.targets => {
            let list = [a.targets, a.utf8, a.text, atom::STRING, a.text_plain];
            inner
                .conn
                .send_void(xproto::change_property32(
                    PropMode::Replace,
                    req.requestor,
                    property,
                    atom::ATOM,
                    &list,
                ))
                .is_ok()
        }
        Some(text) if req.target == a.utf8 || req.target == a.text_plain => serve_value(
            inner,
            sends,
            req.requestor,
            property,
            req.target,
            text.into_bytes(),
        ),
        Some(text) if req.target == a.text => serve_value(
            inner,
            sends,
            req.requestor,
            property,
            a.utf8,
            text.into_bytes(),
        ),
        Some(text) if req.target == atom::STRING => {
            let latin1: Vec<u8> = text
                .chars()
                .map(|c| if (c as u32) <= 0xFF { c as u8 } else { b'?' })
                .collect();
            serve_value(inner, sends, req.requestor, property, atom::STRING, latin1)
        }
        Some(_) => false,
    };
    let notify = SelectionNotify {
        sequence: 0,
        time: req.time,
        requestor: req.requestor,
        selection: req.selection,
        target: req.target,
        property: if ok { property } else { NONE },
    };
    let _ = inner.conn.send_void(xproto::send_event(
        false,
        req.requestor,
        event_mask::NO_EVENT,
        &notify.to_bytes(),
    ));
    let _ = inner.conn.flush();
}

fn serve_value(
    inner: &Inner,
    sends: &mut OutgoingSends,
    requestor: Window,
    property: Atom,
    type_: Atom,
    bytes: Vec<u8>,
) -> bool {
    let conn = &inner.conn;
    if bytes.len() <= inner.incr_chunk() {
        return conn
            .send_void(xproto::change_property(
                PropMode::Replace,
                requestor,
                property,
                type_,
                8,
                &bytes,
            ))
            .is_ok();
    }
    let size = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    if sends
        .start((requestor, property), type_, bytes, Instant::now())
        .is_none()
    {
        return false;
    }
    let started = conn
        .send_void(xproto::change_window_attributes(
            requestor,
            &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
        ))
        .is_ok()
        && conn
            .send_void(xproto::change_property32(
                PropMode::Replace,
                requestor,
                property,
                inner.atoms.incr,
                &[size],
            ))
            .is_ok();
    if !started {
        sends.remove((requestor, property));
    }
    started
}

fn send_chunk(inner: &Inner, sends: &mut OutgoingSends, window: Window, property: Atom) {
    let limit = inner.incr_chunk();
    let Some(chunk) = sends.chunk((window, property), limit, Instant::now()) else {
        return;
    };
    let sent = inner
        .conn
        .send_void(xproto::change_property(
            PropMode::Append,
            window,
            property,
            chunk.type_,
            8,
            &chunk.bytes,
        ))
        .is_ok();
    let _ = inner.conn.flush();
    sends.sent(&chunk, sent, Instant::now());
}

fn take_answer(inner: &Inner, ev: &SelectionNotify) {
    let mut reads = inner.reads.lock().unwrap();
    let Some(read) = reads.active.get_mut(&ev.requestor) else {
        return;
    };
    if !read.key.accepts(ev) || read.incr.is_some() {
        return;
    }
    read.moved = Instant::now();
    if ev.property == NONE {
        reads.end(ev.requestor, PasteOutcome::Unsupported);
        return;
    }
    let outcome = match inner.take_property(ev.requestor, read.key.property) {
        None => PasteOutcome::Timeout,
        Some(reply) if reply.type_ == inner.atoms.incr => {
            read.incr = Some(Incr {
                property: read.key.property,
                type_: atom::ANY,
                bytes: Vec::new(),
            });
            return;
        }
        Some(reply) if reply.value.is_empty() => PasteOutcome::Unsupported,
        Some(reply) => decode(reply.type_, reply.value),
    };
    reads.end(ev.requestor, outcome);
}

fn decode(type_: Atom, bytes: Vec<u8>) -> PasteOutcome {
    if type_ == atom::STRING {
        PasteOutcome::Text(bytes.iter().map(|&b| b as char).collect())
    } else {
        String::from_utf8(bytes).map_or(PasteOutcome::Unsupported, PasteOutcome::Text)
    }
}

fn take_chunk(inner: &Inner, win: Window, property: Atom) {
    let mut reads = inner.reads.lock().unwrap();
    let Some(read) = reads.active.get_mut(&win) else {
        return;
    };
    let Some(incr) = read.incr.as_mut().filter(|incr| incr.property == property) else {
        return;
    };
    let Some(chunk) = inner.take_property(win, property) else {
        reads.end(win, PasteOutcome::Timeout);
        return;
    };
    read.moved = Instant::now();
    if chunk.value.is_empty() {
        let outcome = decode(incr.type_, std::mem::take(&mut incr.bytes));
        reads.end(win, outcome);
    } else if incr.bytes.len().saturating_add(chunk.value.len()) > MAX_PASTE_BYTES {
        reads.end(win, PasteOutcome::Timeout);
    } else {
        incr.type_ = chunk.type_;
        incr.bytes.extend_from_slice(&chunk.value);
    }
}

#[cfg(test)]
#[path = "clipboard_x11_tests.rs"]
mod tests;
