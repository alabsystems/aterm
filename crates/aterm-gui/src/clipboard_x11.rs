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
//! briefly for the reply.
//!
//! Scope: UTF-8 text. Serving is single-shot (no INCR), which covers ordinary terminal copies;
//! pasting also accepts INCR. Without `$DISPLAY` (or without libxcb)
//! [`X11Clipboard::get_handle`] returns `None`.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_x11::Connection;
use aterm_x11::proto::event::{Event, SelectionNotify, SelectionRequest, property as notify_state};
use aterm_x11::proto::xproto::{self, PropMode, cw, event_mask, window_class};
use aterm_x11::proto::{Atom, CURRENT_TIME, NONE, ValueList, Window, atom};

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
/// unresponsive" (stop, don't multiply the 1 s wait across targets).
enum PasteOutcome {
    Text(String),
    Unsupported,
    Timeout,
}

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
    /// The property on our own window that `ConvertSelection` deposits a paste into.
    recv: Atom,
    /// The `INCR` atom: an owner answering a large paste with a property of
    /// this type streams the data in chunks via `PropertyNotify` (ICCCM INCR).
    incr: Atom,
}

/// Shared state between the event-serving thread and the `set`/`get` callers. libxcb
/// is thread-safe: a caller thread may send requests (set owner, convert) and wait for
/// a reply (`set`'s ownership check) while the event thread blocks in `wait_for_event`,
/// which is exactly the access pattern here.
struct Inner {
    conn: Connection,
    win: Window,
    atoms: Atoms,
    /// Text we currently serve for CLIPBOARD (the selection we own), or `None`.
    clipboard: Mutex<Option<String>>,
    /// Text we currently serve for PRIMARY, or `None`.
    primary: Mutex<Option<String>>,
    /// A `get` awaiting its `SelectionNotify`; the event thread fulfils it.
    pending: Mutex<Option<Sender<Option<String>>>>,
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
                &ValueList::new().set(cw::EVENT_MASK, event_mask::PROPERTY_CHANGE),
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
            conn,
            win,
            atoms,
            clipboard: Mutex::new(None),
            primary: Mutex::new(None),
            pending: Mutex::new(None),
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
    /// target; a `Timeout` stops, so callers never block more than ~1 s.
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

    /// One `ConvertSelection` round-trip for `target`, bounded by the ~1 s timeout.
    /// The serve thread decodes the reply (by property type) and sends it back.
    fn convert_once(&self, selection: Atom, target: Atom) -> PasteOutcome {
        let (tx, rx) = channel();
        *self.inner.pending.lock().unwrap() = Some(tx);
        let req = self.inner.conn.send_void(xproto::convert_selection(
            self.inner.win,
            selection,
            target,
            self.inner.atoms.recv,
            CURRENT_TIME,
        ));
        if req.is_err() || self.inner.conn.flush().is_err() {
            *self.inner.pending.lock().unwrap() = None;
            return PasteOutcome::Timeout; // connection/write broken — do not retry
        }
        let outcome = match rx.recv_timeout(Duration::from_millis(1000)) {
            Ok(Some(t)) => PasteOutcome::Text(t),
            Ok(None) => PasteOutcome::Unsupported, // owner replied property=NONE
            Err(_) => PasteOutcome::Timeout,       // owner hung — stop the fallback
        };
        *self.inner.pending.lock().unwrap() = None;
        outcome
    }

    fn sel_atom(&self, sel: Sel) -> Atom {
        match sel {
            Sel::Clipboard => self.inner.atoms.clipboard,
            Sel::Primary => self.inner.atoms.primary,
        }
    }
}

/// The serving thread: answer conversion requests for selections we own and fulfil
/// our own pending `get`. Exits only if the connection dies.
fn serve(inner: &Inner) {
    loop {
        let ev = match inner.conn.wait_for_event() {
            Ok(packet) => match Event::parse(&packet) {
                Ok(ev) => ev,
                Err(_) => continue, // a packet that does not parse: skip it
            },
            Err(_) => return, // connection gone — nothing more to serve
        };
        match ev {
            Event::SelectionRequest(req) => serve_request(inner, &req),
            Event::SelectionClear(clear) => {
                // Another client took the selection; stop serving our stale copy.
                if let Some(slot) = inner.slot(clear.selection) {
                    *slot.lock().unwrap() = None;
                }
            }
            Event::SelectionNotify(ev) => {
                let text = if ev.property == NONE {
                    None
                } else {
                    read_paste(inner, ev.property)
                };
                if let Some(tx) = inner.pending.lock().unwrap().take() {
                    let _ = tx.send(text);
                }
            }
            _ => {}
        }
    }
}

/// Answer one `SelectionRequest`: place the requested data on the requestor's
/// property and reply with a `SelectionNotify` (property = `NONE` on refusal).
fn serve_request(inner: &Inner, req: &SelectionRequest) {
    let a = &inner.atoms;
    // Obsolete clients send property = None; the convention is to use `target`.
    let property = if req.property == NONE {
        req.target
    } else {
        req.property
    };
    let text = inner
        .slot(req.selection)
        .and_then(|slot| slot.lock().unwrap().clone());
    let mut reported = property;
    let mut ok = false;
    if let Some(text) = text {
        if req.target == a.utf8 || req.target == a.text_plain {
            // Native UTF-8 carriers: bytes + property type == the requested target.
            ok = inner
                .conn
                .send_void(xproto::change_property(
                    PropMode::Replace,
                    req.requestor,
                    property,
                    req.target,
                    8,
                    text.as_bytes(),
                ))
                .is_ok();
        } else if req.target == a.text {
            // TEXT is polymorphic (owner picks the encoding); we answer UTF-8 and
            // label the property UTF8_STRING so the requestor decodes it correctly.
            ok = inner
                .conn
                .send_void(xproto::change_property(
                    PropMode::Replace,
                    req.requestor,
                    property,
                    a.utf8,
                    8,
                    text.as_bytes(),
                ))
                .is_ok();
        } else if req.target == atom::STRING {
            // ICCCM: STRING is ISO-8859-1 (Latin-1), not UTF-8. Characters
            // outside Latin-1 become '?'.
            let latin1: Vec<u8> = text
                .chars()
                .map(|c| if (c as u32) <= 0xFF { c as u8 } else { b'?' })
                .collect();
            ok = inner
                .conn
                .send_void(xproto::change_property(
                    PropMode::Replace,
                    req.requestor,
                    property,
                    req.target,
                    8,
                    &latin1,
                ))
                .is_ok();
        } else if req.target == a.targets {
            let list = [a.targets, a.utf8, a.text, atom::STRING, a.text_plain];
            ok = inner
                .conn
                .send_void(xproto::change_property32(
                    PropMode::Replace,
                    req.requestor,
                    property,
                    atom::ATOM,
                    &list,
                ))
                .is_ok();
        }
    }
    if !ok {
        reported = NONE;
    }
    let notify = SelectionNotify {
        sequence: 0,
        time: req.time,
        requestor: req.requestor,
        selection: req.selection,
        target: req.target,
        property: reported,
    };
    let _ = inner.conn.send_void(xproto::send_event(
        false,
        req.requestor,
        event_mask::NO_EVENT,
        &notify.to_bytes(),
    ));
    let _ = inner.conn.flush();
}

/// Read (and delete) the paste text the owner deposited on our `recv` property,
/// single-shot or via ICCCM INCR. `None` for an empty value or a failed or
/// aborted transfer.
fn read_paste(inner: &Inner, property: Atom) -> Option<String> {
    let reply = inner
        .conn
        .send(xproto::get_property(
            true,
            inner.win,
            property,
            atom::ANY,
            0,
            u32::MAX,
        ))
        .ok()?
        .reply()
        .ok()?;
    // Large selection: an INCR-typed property holds a size hint, not the data.
    // Deleting it (`delete=true` above) tells the owner to start streaming
    // chunks via PropertyNotify.
    if reply.type_ == inner.atoms.incr {
        return read_incr(inner, property);
    }
    if reply.value.is_empty() {
        return None;
    }
    // Decode by the property's actual type, per ICCCM: STRING is Latin-1,
    // UTF8_STRING and text/plain;charset=utf-8 are UTF-8. Latin-1 → Unicode is
    // the identity on 0..=0xFF, so `b as char` is exact.
    if reply.type_ == atom::STRING {
        Some(reply.value.iter().map(|&b| b as char).collect())
    } else {
        String::from_utf8(reply.value).ok()
    }
}

/// Drive one ICCCM INCR transfer: the owner appends each chunk to `property`
/// and fires `PropertyNotify` (NewValue); we read and delete each chunk to ack
/// it until a zero-length chunk ends the transfer. Runs on the serve thread, so
/// it also dispatches SelectionRequest/SelectionClear events meanwhile. Bounded
/// by a deadline and a size cap against slow or hostile owners.
fn read_incr(inner: &Inner, property: Atom) -> Option<String> {
    const MAX_BYTES: usize = 64 * 1024 * 1024;
    // Just under the caller's 1000 ms `get` timeout, so even a failed transfer
    // reports back through the channel in time. The UI thread never waits on
    // this: foreign-owner reads run on paste workers (`paste_clipboard_into`,
    // `paste_primary_into`, `search_paste_in`) that post a `Wake` when done.
    let deadline = Instant::now() + Duration::from_millis(900);
    let mut buf: Vec<u8> = Vec::new();
    while Instant::now() < deadline {
        let ev = match inner.conn.poll_for_event() {
            Ok(Some(packet)) => match Event::parse(&packet) {
                Ok(ev) => ev,
                Err(_) => continue,
            },
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(_) => return None, // connection gone
        };
        match ev {
            Event::PropertyNotify(p)
                if p.window == inner.win
                    && p.atom == property
                    && p.state == notify_state::NEW_VALUE =>
            {
                let chunk = inner
                    .conn
                    .send(xproto::get_property(
                        true,
                        inner.win,
                        property,
                        atom::ANY,
                        0,
                        u32::MAX,
                    ))
                    .ok()?
                    .reply()
                    .ok()?;
                if chunk.value.is_empty() {
                    return String::from_utf8(buf).ok(); // zero-length chunk = done
                }
                if buf.len().saturating_add(chunk.value.len()) > MAX_BYTES {
                    return None; // refuse an over-large transfer
                }
                buf.extend_from_slice(&chunk.value);
            }
            // Stay responsive to our own selection during the transfer.
            Event::SelectionRequest(req) => serve_request(inner, &req),
            Event::SelectionClear(clear) => {
                if let Some(slot) = inner.slot(clear.selection) {
                    *slot.lock().unwrap() = None;
                }
            }
            _ => {}
        }
    }
    None // timed out
}

#[cfg(test)]
mod tests {
    use super::*;

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
            crate::logging::stderr_line!("SKIP: no X display for the clipboard round-trip test");
            return;
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
}
