// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A self-contained X11 clipboard (CLIPBOARD + PRIMARY) over a connection of its own
//! (`aterm_x11`, libxcb opened at run time), with no helper binary (`xclip`/`xsel`). It ran
//! over x11rb's pure-Rust connection until LW.6 of docs/AUDIT-third-party-2026-10-15.md; the
//! protocol it speaks is unchanged.
//!
//! X11 has no "set and forget" clipboard: the copying client must own the selection and answer
//! `SelectionRequest` conversions for as long as the data should stay pasteable. One background
//! thread owns an X connection and an unmapped 1×1 window, takes ownership on
//! [`set`](X11Clipboard::set) and serves conversions. [`get`](X11Clipboard::get) returns our
//! stored text when we own the selection, or asks the owner via `ConvertSelection` and waits
//! while the answer keeps coming.
//!
//! Each foreign-owner read asks from a requestor window no other read uses, and the owner's
//! `SelectionNotify` and INCR chunks name that window. Reads that overlap (a middle-click
//! during a large Ctrl+Shift+V, two windows pasting, the OSC 52 prefetch) therefore each get
//! their own answer, and an answer that comes after its read gave up reaches no later read.
//!
//! Scope: UTF-8 text. Both directions speak ICCCM INCR: a copy over [`INCR_CHUNK`] is served in
//! chunks, so one of any size reaches other clients (one request cannot exceed the server's
//! maximum, 16 MiB with BIG-REQUESTS), and a paste accepts one. Without `$DISPLAY` (or without
//! libxcb) [`X11Clipboard::get_handle`] returns `None`.

use std::collections::HashMap;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_x11::Connection;
use aterm_x11::proto::event::{Event, SelectionNotify, SelectionRequest, property as notify_state};
use aterm_x11::proto::xproto::{self, GetPropertyReply, PropMode, cw, event_mask, window_class};
use aterm_x11::proto::{Atom, CURRENT_TIME, NONE, ValueList, Visualid, Window, atom};

/// Which X selection to read/write. `Clipboard` is the explicit-copy buffer
/// (Ctrl+Shift+C / Ctrl+Shift+V); `Primary` is the select-to-copy / middle-click
/// buffer that X users also rely on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sel {
    Clipboard,
    Primary,
}

/// Result of one `ConvertSelection` round-trip, so `get` can distinguish "owner
/// does not offer this target" (advance the ICCCM fallback chain) from "owner is
/// unresponsive" or "the transfer failed" (stop, don't multiply the wait across
/// targets).
enum PasteOutcome {
    Text(String),
    Unsupported,
    Timeout,
}

/// How long a paste may go without moving before it is given up: no reply to
/// the `ConvertSelection`, or no INCR chunk since the last. A stalled owner costs
/// this once; a large transfer runs as long as its chunks keep coming (GTK3
/// streams anything over 256 KiB in INCR chunks, and a fixed budget for the whole
/// transfer dropped large pastes from slow owners without a word).
const PASTE_STALL: Duration = Duration::from_millis(1000);

/// The most an INCR paste may bring, against hostile owners.
const MAX_PASTE_BYTES: usize = 64 * 1024 * 1024;

/// Values longer than this are served by ICCCM INCR, in chunks of this size, not
/// in one ChangeProperty. A request may be no longer than the server's maximum
/// (16 MiB with BIG-REQUESTS, 256 KiB without), and a copy over it reached no
/// other client. GTK draws the line at 256 KiB too. A server whose maximum is
/// smaller gets smaller chunks ([`Inner::incr_chunk`]).
const INCR_CHUNK: usize = 256 * 1024;

/// How long an INCR transfer we serve waits for its requestor to take the last
/// chunk before it is dropped (a requestor that died or gave up).
const SERVE_STALL: Duration = Duration::from_secs(30);

/// The atoms the clipboard protocol needs, interned once at startup.
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

/// One foreign-owner read in flight: a `ConvertSelection` from a requestor
/// window no other read uses, until the owner's whole answer is in.
struct Read {
    key: PasteRequest,
    /// The map entry owns this round's server window until it ends.
    _requestor: Requestor,
    /// When the read last moved: its request, the owner's answer, each INCR
    /// chunk. The waiting `get` gives up [`PASTE_STALL`] after it.
    moved: Instant,
    /// Set once the owner answered INCR: the transfer so far.
    incr: Option<Incr>,
    tx: Sender<PasteOutcome>,
}

/// An INCR transfer a read is in: the property its chunks come on, their type,
/// and the bytes so far.
struct Incr {
    property: Atom,
    type_: Atom,
    bytes: Vec<u8>,
}

/// A value we serve by INCR to one requestor's property: what is left to hand
/// over.
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

/// One bounded chunk, detached from the large retained value before any XCB
/// operation. Completion may advance only this transfer, never a replacement.
struct OutgoingChunk {
    key: (Window, Atom),
    id: u64,
    type_: Atom,
    bytes: Vec<u8>,
    end: usize,
}

/// Outgoing values and their demand-only timer. There is at most one worker,
/// joined before its replacement, and none remains once the table is empty.
/// The owner is local to `serve`, so every exit/unwind closes, wakes and joins
/// it. No table guard crosses XCB I/O, including the owner's final flush.
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

    /// Admit expiry custody before the caller publishes an INCR header. The
    /// spawn capability also lets the existing aggregate exercise refusal.
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
            // A worker marks itself stopped while holding this mutex. Join it
            // without the mutex before starting another, even at that edge.
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
        // The newly spawned worker cannot see an empty table between its
        // admission and this insertion: it must first acquire this guard.
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
        // A synchronous XCB call may have returned after the timer expired it.
        // Neither that completion nor a later delete revives an expired value.
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

/// A panic in the timer must not leave a table claiming it has expiry custody.
/// This guard is armed on the worker stack, after spawn, so spawn refusal can
/// never run its cleanup while the admitting thread holds the table mutex.
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

/// Wait only for the earliest real expiry or a table change. The explicit
/// clock/park observation lets the existing aggregate advance this same loop
/// without a 30-second sleep or any X event; production supplies Instant::now.
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

/// One round's wire identity. A refusal has no property; every other reply
/// must name the property the round requested as well as its window and target.
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

/// Every round owns a fresh server window on the retained connection. Never
/// reusing its XID keeps a delayed reply separate even after a completed read.
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
    /// Refuse that wrap instead of assigning an old wire identity a new read.
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

/// Shared state between the event-serving thread and the `set`/`get` callers. libxcb
/// is thread-safe: a caller thread may send requests (set owner, convert) and wait for
/// a reply (`set`'s ownership check) while the event thread blocks in `wait_for_event`,
/// which is exactly the access pattern here.
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
    /// The stored-text slot for `sel`.
    fn slot(&self, sel: Atom) -> Option<&Mutex<Option<String>>> {
        if sel == self.atoms.clipboard {
            Some(&self.clipboard)
        } else if sel == self.atoms.primary {
            Some(&self.primary)
        } else {
            None
        }
    }

    /// A fresh unmapped requestor that reports its property changes. Advance
    /// the identity fence before creation: even a failed send cannot admit it
    /// as another round later.
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
                    0, // CopyFromParent depth
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

    /// The INCR chunk size, and the longest value served in one property:
    /// [`INCR_CHUNK`], or less where the server's longest request is shorter
    /// (room left for ChangeProperty's header).
    fn incr_chunk(&self) -> usize {
        INCR_CHUNK
            .min(self.conn.maximum_request_bytes().saturating_sub(64))
            .max(4096)
    }

    /// Read and delete `property` on our requestor window `win`, whole.
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

/// Unregister the active round on every caller exit, including unwind. The
/// removed entry owns its window, and is dropped after releasing the map lock.
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

/// A live X11 clipboard: an owning thread + the shared state it serves.
pub(crate) struct X11Clipboard {
    inner: std::sync::Arc<Inner>,
}

impl X11Clipboard {
    /// The process-wide clipboard, connected lazily. `Some` on an X11 session,
    /// `None` when there is no X display to connect to (e.g. a pure-Wayland or
    /// headless session) — callers then degrade gracefully.
    pub(crate) fn get_handle() -> Option<&'static X11Clipboard> {
        static INSTANCE: OnceLock<Option<X11Clipboard>> = OnceLock::new();
        INSTANCE.get_or_init(X11Clipboard::connect).as_ref()
    }

    /// Connect, intern atoms, create the owner window, and spawn the serving
    /// thread. `None` if any step fails (no display, protocol error).
    fn connect() -> Option<X11Clipboard> {
        Self::connect_with_selections(b"CLIPBOARD", None)
    }

    /// Connect as [`connect`](Self::connect), but intern `clipboard_name` for the
    /// CLIPBOARD-role selection and, when `primary_override` is `Some`, that name
    /// for the PRIMARY-role selection. Production passes `b"CLIPBOARD"` / `None`;
    /// tests pass private per-process names so a clipboard manager on the real
    /// display never competes for the atoms under test. The wire protocol is
    /// identical.
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
                0, // CopyFromParent depth
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
        // Detached: lives for the process. The OS reclaims the connection on exit.
        std::thread::Builder::new()
            .name("aterm-x11-clipboard".into())
            .spawn(move || serve(&thread_inner))
            .ok()?;
        Some(X11Clipboard { inner })
    }

    /// Take ownership of `sel` and serve `text` to any client that pastes it.
    /// Returns whether this client owns `sel` once the server has processed the
    /// request: ICCCM 2.1 has the owner confirm with `GetSelectionOwner`, and
    /// that round trip also flushes the request, so a paste from any client
    /// after `set` returns reaches this owner.
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
    /// Fast-pathed to our own stored text when we own the selection.
    ///
    /// ICCCM target fallback: prefer `UTF8_STRING`, then `TEXT`/`STRING` for
    /// legacy owners. Only a fast NONE reply (`Unsupported`) advances to the next
    /// target; a `Timeout` stops, so a stalled owner costs ~1 s ([`PASTE_STALL`])
    /// while a large INCR transfer takes as long as it keeps moving.
    pub(crate) fn get(&self, sel: Sel) -> Option<String> {
        let atom = self.sel_atom(sel);
        if let Some(t) = self.get_owned(sel) {
            return Some(t);
        }
        for target in [self.inner.atoms.utf8, self.inner.atoms.text, atom::STRING] {
            match self.convert_once(atom, target) {
                PasteOutcome::Text(t) => return Some(t),
                PasteOutcome::Unsupported => continue,
                PasteOutcome::Timeout => return None,
            }
        }
        None
    }

    /// The non-blocking fast path of [`get`](Self::get): the text we stored when
    /// we own `sel`, or `None`, never a `ConvertSelection` round-trip. Safe on the
    /// UI thread; a foreign owner needs the blocking [`get`](Self::get) off-thread.
    pub(crate) fn get_owned(&self, sel: Sel) -> Option<String> {
        let atom = self.sel_atom(sel);
        self.inner
            .slot(atom)
            .and_then(|slot| slot.lock().unwrap().clone())
    }

    /// One `ConvertSelection` round-trip for `target`, from a requestor window
    /// of this read's own, given up once it goes [`PASTE_STALL`] without moving.
    /// The serve thread decodes the reply (by property type) and sends it back.
    /// The UI thread never waits on this: foreign-owner reads run on paste
    /// workers (`paste_clipboard_into`, `paste_primary_into`, `search_paste_in`)
    /// that post a `Wake` when done.
    fn convert_once(&self, selection: Atom, target: Atom) -> PasteOutcome {
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
            return PasteOutcome::Timeout; // connection/write broken — do not retry
        }
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(outcome) => return outcome,
                Err(RecvTimeoutError::Disconnected) => return PasteOutcome::Timeout,
                Err(RecvTimeoutError::Timeout) => {
                    let reads = inner.reads.lock().unwrap();
                    match reads.active.get(&win) {
                        // Ended between the wait and the lock: the outcome is sent.
                        None => return rx.try_recv().unwrap_or(PasteOutcome::Timeout),
                        // The owner hung, or its transfer stalled: give the read
                        // up and stop the fallback. Its window goes with it, so a
                        // late answer finds no read.
                        Some(read) if read.moved.elapsed() >= PASTE_STALL => {
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

/// The serving thread: answer conversion requests for selections we own and fulfil
/// our own reads in flight. Exits only if the connection dies.
fn serve(inner: &Inner) {
    // The timer owns expiry even while libxcb waits with no new X event. It
    // never reads this connection: another reply waiter may queue an event
    // internally, so polling its raw fd with a deadline would lose that wake.
    let mut sends = OutgoingSends::default();
    loop {
        let ev = match inner.conn.wait_for_event() {
            Ok(packet) => match Event::parse(&packet) {
                Ok(ev) => ev,
                Err(_) => continue, // a packet that does not parse: skip it
            },
            Err(_) => return, // connection gone — nothing more to serve
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

/// Answer one `SelectionRequest`: place the requested data on the requestor's
/// property and reply with a `SelectionNotify` (property = `NONE` on refusal).
fn serve_request(inner: &Inner, sends: &mut OutgoingSends, req: &SelectionRequest) {
    let a = &inner.atoms;
    // Obsolete clients send property = None; the convention is to use `target`.
    let property = if req.property == NONE {
        req.target
    } else {
        req.property
    };
    // A new request on the property ends any transfer still going to it.
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
        // Native UTF-8 carriers: bytes + property type == the requested target.
        Some(text) if req.target == a.utf8 || req.target == a.text_plain => serve_value(
            inner,
            sends,
            req.requestor,
            property,
            req.target,
            text.into_bytes(),
        ),
        // TEXT is polymorphic (owner picks the encoding); we answer UTF-8 and
        // label the property UTF8_STRING so the requestor decodes it correctly.
        Some(text) if req.target == a.text => serve_value(
            inner,
            sends,
            req.requestor,
            property,
            a.utf8,
            text.into_bytes(),
        ),
        // ICCCM: STRING is ISO-8859-1 (Latin-1), not UTF-8. Characters
        // outside Latin-1 become '?'.
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

/// Put `bytes` of type `type_` on the requestor's `property`: in one
/// ChangeProperty when they fit in one chunk ([`Inner::incr_chunk`]), else by
/// ICCCM INCR (2.7.2): an `INCR` property holding the size now, then a chunk
/// each time the requestor deletes the property ([`send_chunk`]), then a
/// zero-length one. `false` when the request cannot be sent.
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
    // The requestor's deletes are the cue for each chunk: select them before
    // the INCR property goes up, so the first cannot be missed. The event mask
    // is this client's own on that window; the requestor's stays as it was.
    let size = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    if sends
        .start((requestor, property), type_, bytes, Instant::now())
        .is_none()
    {
        // No expiry custody: refuse before publishing the INCR header.
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

/// The requestor deleted `property` on `window`. If we serve it by INCR, that
/// is the cue for the next chunk, appended to the property, or, once every byte
/// is out, for the zero-length chunk that ends the transfer.
fn send_chunk(inner: &Inner, sends: &mut OutgoingSends, window: Window, property: Atom) {
    let limit = inner.incr_chunk();
    let Some(chunk) = sends.chunk((window, property), limit, Instant::now()) else {
        return;
    };
    // Only this bounded chunk crosses the XCB call. Expiry can drop the large
    // retained value even if a synchronous transport operation stops returning.
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

/// The owner's `SelectionNotify`: the answer to the read that asked from
/// `ev.requestor`, if that read still waits for an answer for this selection
/// and target. Any other is the answer to a read that was given up, and is
/// dropped unread. An empty or undecodable value is `Unsupported` (the next
/// target may serve), a failed read `Timeout`.
fn take_answer(inner: &Inner, ev: &SelectionNotify) {
    let mut reads = inner.reads.lock().unwrap();
    let Some(read) = reads.active.get_mut(&ev.requestor) else {
        return;
    };
    if !read.key.accepts(ev) || read.incr.is_some() {
        return;
    }
    read.moved = Instant::now();
    // The owner replied property=NONE: it does not offer the target.
    if ev.property == NONE {
        reads.end(ev.requestor, PasteOutcome::Unsupported);
        return;
    }
    let outcome = match inner.take_property(ev.requestor, read.key.property) {
        None => PasteOutcome::Timeout,
        // Large selection: an INCR-typed property holds a size hint, not the
        // data. Deleting it told the owner to stream chunks (`take_chunk`).
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

/// Decode a paste by its property's type, per ICCCM: STRING is Latin-1,
/// UTF8_STRING and text/plain;charset=utf-8 are UTF-8. Latin-1 → Unicode is the
/// identity on 0..=0xFF, so `b as char` is exact.
fn decode(type_: Atom, bytes: Vec<u8>) -> PasteOutcome {
    if type_ == atom::STRING {
        PasteOutcome::Text(bytes.iter().map(|&b| b as char).collect())
    } else {
        String::from_utf8(bytes).map_or(PasteOutcome::Unsupported, PasteOutcome::Text)
    }
}

/// A new value on one of our requestor windows: the next chunk of the ICCCM
/// INCR transfer its read is in, if it is in one. The owner appends each chunk
/// to the property; reading and deleting it asks for the next, and a
/// zero-length chunk ends the transfer, decoded by the chunks' own type. A
/// transfer over [`MAX_PASTE_BYTES`] fails; a stalled one is the waiting
/// `get`'s to give up. Every ended round destroys its requestor and its XID is
/// never reused, since its owner may still answer it.
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
mod tests {
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
        // Even after a completed round, the retained client's allocator may
        // never admit its old id again (including a lower XC-MISC range).
        let mut reads = Reads::default();
        assert!(reads.admit_window(100));
        assert!(!reads.admit_window(100));
        assert!(!reads.admit_window(99));
        assert!(reads.admit_window(101));
        assert!(!reads.admit_window(100));
    }

    /// Cross-client round trip: two handles (each its own connection and owner window); `a`
    /// copies and `b` pastes through the full selection protocol, as another app would. Skipped
    /// without an X display.
    #[test]
    fn cross_client_clipboard_round_trip() {
        // Private per-process selection names keep a real clipboard manager from
        // racing us for CLIPBOARD/PRIMARY; the protocol is identical. Both handles
        // share the names so `b` reads what `a` owns.
        let clip_name = format!("ATERM_TEST_CLIPBOARD_{}", std::process::id());
        let prim_name = format!("ATERM_TEST_PRIMARY_{}", std::process::id());
        let connect = || {
            X11Clipboard::connect_with_selections(clip_name.as_bytes(), Some(prim_name.as_bytes()))
        };
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
        // PRIMARY is an independent buffer (select-to-copy / middle-click).
        let prim = "primary selection text";
        assert!(a.set(Sel::Primary, prim));
        assert_eq!(b.get(Sel::Primary).as_deref(), Some(prim));
        // CLIPBOARD is unchanged by the PRIMARY write (the two never alias).
        assert_eq!(b.get(Sel::Clipboard).as_deref(), Some(payload));
    }

    /// A foreign owner: a second connection whose unmapped window owns the
    /// selection `name` (a private one, so the display's real clipboard is
    /// never touched), answering by hand as another app would.
    struct Owner {
        conn: Connection,
        utf8: Atom,
        incr: Atom,
        stop: Arc<AtomicBool>,
    }

    /// Prearmed inside the scope: cancellation runs before its implicit joins
    /// on every assertion/unwind, so an owner never waits on an abandoned read.
    struct CancelOwners(Arc<AtomicBool>);

    impl Drop for CancelOwners {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    /// Another app: a second connection with an unmapped window of its own.
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

    /// The next event on `conn`. A minute without one is a hang.
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

        /// Hear the property changes on `window` (a requestor's).
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

        /// Normal completion fences the last answer before disconnect, like
        /// XCloseDisplay. Unwind only disconnects; it never adds a blocking
        /// round trip while the cancellation guard is trying to join owners.
        fn finish(&self) {
            self.conn
                .send(xproto::get_input_focus())
                .and_then(|cookie| cookie.reply())
                .expect("the server read the owner's answer");
        }
    }

    /// The owner side of an ICCCM INCR transfer, slow on purpose: answer one
    /// `UTF8_STRING` request with an INCR property, then hand `payload` over in
    /// `chunk`-byte pieces, waiting `gap` before each, and end with a
    /// zero-length piece. Each piece goes out once the requestor deleted the
    /// last (ICCCM 2.7.2). `after_first` runs once the first piece is out.
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

    /// A paste that comes in INCR chunks is read whole however long the
    /// transfer takes, as long as it keeps moving: 2 MB in 64 KiB pieces with
    /// 40 ms before each (about 1.3 s), where the reader's fixed 900 ms budget
    /// for the whole transfer pasted nothing (GTK3 streams anything over
    /// 256 KiB this way). The negative control: the read's stall clock
    /// (`Read::moved`) left at the transfer's start, and the paste comes back
    /// empty. Skipped without an X display.
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

    /// Reads that overlap each get their own answer: a middle-click (PRIMARY)
    /// while a large Ctrl+Shift+V paste (CLIPBOARD) streams in INCR chunks. The
    /// CLIPBOARD owner holds its transfer after the first chunk until the
    /// PRIMARY read is done, so the two overlap. Skipped without an X display.
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

    /// An answer that comes after its read gave up reaches no later read: the
    /// owner sits on the first request until the reader gives up and asks
    /// again, then answers both. An owner answering two requests on one
    /// window's one property waits for the reader to take the first answer, or
    /// the second overwrites it unread; this one does too. Skipped without an X
    /// display.
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
            let first = reader.get(Sel::Clipboard);
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

    /// The type of the property the owner of the selection `name` answers a
    /// `UTF8_STRING` request with, asked by another app: the target's own type
    /// for a value sent whole, INCR for one that follows in chunks, `NONE` for a
    /// refusal. The transfer is taken no further.
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

    /// A copy longer than the server's longest request (16 MiB here) reaches
    /// another client by INCR, where its one ChangeProperty failed and every
    /// other app, the desktop's clipboard manager too, got nothing. A small
    /// copy still goes out in one property. Skipped without an X display.
    #[test]
    fn a_copy_over_the_request_limit_is_served_by_incr() {
        use std::fmt::Write as _;
        outgoing_values_expire_without_an_x_event();
        let clip_name = format!("ATERM_TEST_SERVE_CLIPBOARD_{}", std::process::id());
        let prim_name = format!("ATERM_TEST_SERVE_PRIMARY_{}", std::process::id());
        let connect = || {
            X11Clipboard::connect_with_selections(clip_name.as_bytes(), Some(prim_name.as_bytes()))
        };
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

    /// Exercise the shipping table and timer with a controlled monotonic clock.
    /// Every wait is a one-minute hang detector, never a short timing budget;
    /// no X connection/event or elapsed 30-second wait advances this clock.
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
            // Match the worker's lock order. Acquiring state also proves the
            // announced park released its guard for Condvar::wait_timeout.
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

        // A later copy/transfer owns different bytes; advancing the old one
        // still serves its retained snapshot, rather than the new contents.
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

        // Demand starts one replacement only after the old worker is joined.
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

        // The serving-loop owner's actual Drop must wake and join a timer
        // parked on a future deadline, even during an unwind. The outer guard
        // closes it on a failed hang detector before the scope joins the owner.
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

        // An unexpected worker unwind poisons the actual table guard. Its
        // stack guard must retire the payload/running claim before this
        // prearmed completion witness fires; next admission joins that failed
        // worker and recovers the same poisoned table, without reusing its id.
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
}
