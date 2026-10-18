// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-bus xdnd <window> <uri> <marks>`: a drag source of the XDND protocol
//! (version 5, freedesktop.org's "Drag-and-Drop Protocol for the X Window
//! System") that drags one file over the X window `<window>` of the display
//! `DISPLAY` names, twice:
//!
//! 1. a hover that leaves: `XdndEnter`, `XdndPosition` at the window's
//!    centre, the data the target asks for (`<uri>` as `text/uri-list`), a
//!    second `XdndPosition`, then `XdndLeave`;
//! 2. a hover that drops: the same, then `XdndDrop`, and the target's
//!    `XdndFinished`.
//!
//! The target answers every `XdndPosition` with an `XdndStatus`, and handles
//! one client's events in the order the server delivered them, so an answer
//! says the target has handled everything sent before it: a hover's second
//! status follows the data it read, and the drop's first status follows the
//! leave (its own data still unanswered). At those two points the source
//! creates `<marks>/hovering` and `<marks>/left` and waits for
//! `<marks>/hovering.seen` and `<marks>/left.seen`, so the lab can photograph
//! the window between. Each answer is a line `<key> <value>` in
//! `<marks>/report`, also printed.
//!
//! Only the part of the X11 core protocol (X Window System Protocol, X11R7.7) a
//! source needs, spoken directly: a little-endian client on the display's unix
//! socket, with no authorization (the lane's Xvfb and Xwayland run without
//! `-auth`). Not x11rb: the stack this records is the one LW.6-7 replace
//! (docs/AUDIT-third-party-2026-10-15.md), and the probe must not be built
//! from it. Every byte the server sends is untrusted: a short or malformed
//! reply is an error, never a panic.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{ErrorKind, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A wait for an answer that must come: a hang detector, never a budget.
const HANG: Duration = Duration::from_secs(60);

/// A wait for the lab's photograph. The lab's own waits are one-minute hang
/// detectors, so this one is longer.
const LAB: Duration = Duration::from_secs(600);

/// How often a wait for a marker polls, and how long one socket read blocks.
const POLL: Duration = Duration::from_millis(100);

/// The largest reply this source accepts (a property of a few atoms is all it
/// reads); a larger length is a malformed reply.
const MAX_REPLY: usize = 1 << 20;

/// The XDND version this source speaks.
const VERSION: u32 = 5;

/// What went wrong: no X server to speak to (the lane reads NOT RUN), or a
/// fault in the drag.
#[derive(Debug)]
pub(crate) enum Error {
    NoServer(String),
    Fault(String),
}

pub(crate) fn fault(why: impl Into<String>) -> Error {
    Error::Fault(why.into())
}

/// The atoms the source names, interned in this order.
const NAMES: [&str; 15] = [
    "XdndAware",
    "XdndEnter",
    "XdndPosition",
    "XdndStatus",
    "XdndLeave",
    "XdndDrop",
    "XdndFinished",
    "XdndSelection",
    "text/uri-list",
    "XdndActionCopy",
    "XdndActionPrivate",
    "XdndActionMove",
    "XdndActionLink",
    "XdndActionAsk",
    "TARGETS",
];

/// The core protocol's predefined `ATOM` atom (a property's type).
const ATOM_ATOM: u32 = 4;

/// `NAMES`, interned.
struct Atoms(Vec<(u32, &'static str)>);

impl Atoms {
    fn id(&self, name: &str) -> u32 {
        self.0
            .iter()
            .find(|(_, n)| *n == name)
            .map_or(0, |(a, _)| *a)
    }

    /// An atom's name: one of `NAMES`, `None` for 0, else `atom<n>`.
    fn name(&self, atom: u32) -> String {
        if atom == 0 {
            return "None".into();
        }
        self.0
            .iter()
            .find(|(a, _)| *a == atom)
            .map_or_else(|| format!("atom{atom}"), |(_, n)| (*n).to_string())
    }
}

pub(crate) fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    let mut w = [0u8; 4];
    w.copy_from_slice(s);
    Some(u32::from_le_bytes(w))
}

fn i16_at(b: &[u8], at: usize) -> Option<i16> {
    u16_at(b, at).map(|v| i16::from_le_bytes(v.to_le_bytes()))
}

/// `n` rounded up to a multiple of four: the protocol pads every list.
fn pad4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// The unix socket of an X display name (`:N` or `:N.S`).
fn socket_path(display: &str) -> Option<PathBuf> {
    let rest = display.strip_prefix(':')?;
    let number = rest.split('.').next()?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        .then(|| PathBuf::from(format!("/tmp/.X11-unix/X{number}")))
}

/// The connection setup a little-endian client sends: protocol 11.0, no
/// authorization.
const SETUP: [u8; 12] = [b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// What the setup reply says that a source needs.
#[derive(Debug, PartialEq, Eq)]
struct Setup {
    id_base: u32,
    id_mask: u32,
    /// The first screen's root window.
    root: u32,
}

/// Read a whole setup reply (its 8-byte head, then the head's length in
/// words).
fn parse_setup(reply: &[u8]) -> Result<Setup, String> {
    match reply.first() {
        Some(1) => {}
        Some(0) => {
            let n = usize::from(*reply.get(1).unwrap_or(&0));
            let why = reply
                .get(8..8 + n)
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            return Err(format!("the X server refused the connection: {why}"));
        }
        Some(2) => {
            return Err(
                "the X server asks for authentication, which this probe does not speak".into(),
            );
        }
        _ => return Err("a malformed setup reply".into()),
    }
    let short = || "a short setup reply".to_string();
    let id_base = u32_at(reply, 12).ok_or_else(short)?;
    let id_mask = u32_at(reply, 16).ok_or_else(short)?;
    let vendor = usize::from(u16_at(reply, 24).ok_or_else(short)?);
    let screens = *reply.get(28).ok_or_else(short)?;
    let formats = usize::from(*reply.get(29).ok_or_else(short)?);
    if screens == 0 {
        return Err("the X server has no screen".into());
    }
    let root = u32_at(reply, 40 + pad4(vendor) + 8 * formats).ok_or_else(short)?;
    if id_mask == 0 {
        return Err("the X server gave no resource ids".into());
    }
    Ok(Setup {
        id_base,
        id_mask,
        root,
    })
}

/// A request: its opcode, its one-byte data field, and its body after the
/// 4-byte head (padded here to whole words).
pub(crate) fn request(opcode: u8, data: u8, body: &[u8]) -> Vec<u8> {
    let words = (4 + pad4(body.len())) / 4;
    let mut r = Vec::with_capacity(words * 4);
    r.push(opcode);
    r.push(data);
    r.extend_from_slice(&u16::try_from(words).unwrap_or(u16::MAX).to_le_bytes());
    r.extend_from_slice(body);
    r.resize(words * 4, 0);
    r
}

pub(crate) fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// `InternAtom` (16), `only-if-exists` false.
fn intern_atom(name: &str) -> Vec<u8> {
    let len = u16::try_from(name.len()).unwrap_or(u16::MAX);
    let mut body = len.to_le_bytes().to_vec();
    body.extend_from_slice(&[0, 0]);
    body.extend_from_slice(name.as_bytes());
    request(16, 0, &body)
}

/// `CreateWindow` (1): an unmapped 1x1 `InputOnly` child of `parent`.
fn create_window(wid: u32, parent: u32) -> Vec<u8> {
    let mut body = words(&[wid, parent]);
    // x, y, width, height, border-width, class (InputOnly = 2).
    for v in [0u16, 0, 1, 1, 0, 2] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    // visual (CopyFromParent), value-mask (none).
    body.extend_from_slice(&words(&[0, 0]));
    request(1, 0, &body)
}

/// `GetProperty` (20) of `property` on `window`, any type, its first 1024
/// words, not deleted.
fn get_property(window: u32, property: u32) -> Vec<u8> {
    request(20, 0, &words(&[window, property, 0, 0, 1024]))
}

/// `ChangeProperty` (18), mode Replace, format 8.
fn change_property(window: u32, property: u32, type_: u32, data: &[u8]) -> Vec<u8> {
    let mut body = words(&[window, property, type_]);
    body.extend_from_slice(&[8, 0, 0, 0]);
    body.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    body.extend_from_slice(data);
    request(18, 0, &body)
}

/// `SetSelectionOwner` (22) at `CurrentTime`.
fn set_selection_owner(owner: u32, selection: u32) -> Vec<u8> {
    request(22, 0, &words(&[owner, selection, 0]))
}

/// `SendEvent` (25) with an empty event mask, `propagate` false: the server
/// sends `event` to the client that created `destination`.
fn send_event(destination: u32, event: &[u8; 32]) -> Vec<u8> {
    let mut body = words(&[destination, 0]);
    body.extend_from_slice(event);
    request(25, 0, &body)
}

/// `GetGeometry` (14).
pub(crate) fn get_geometry(drawable: u32) -> Vec<u8> {
    request(14, 0, &words(&[drawable]))
}

/// `TranslateCoordinates` (40) of `(x, y)` from `src` to `dst`.
fn translate_coordinates(src: u32, dst: u32, x: i16, y: i16) -> Vec<u8> {
    let mut body = words(&[src, dst]);
    body.extend_from_slice(&x.to_le_bytes());
    body.extend_from_slice(&y.to_le_bytes());
    request(40, 0, &body)
}

/// A `ClientMessage` event (33), format 32.
fn client_message(window: u32, type_: u32, data: [u32; 5]) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0] = 33;
    e[1] = 32;
    e[4..8].copy_from_slice(&window.to_le_bytes());
    e[8..12].copy_from_slice(&type_.to_le_bytes());
    for (i, d) in data.iter().enumerate() {
        e[12 + 4 * i..16 + 4 * i].copy_from_slice(&d.to_le_bytes());
    }
    e
}

/// A `SelectionNotify` event (31). `property` 0 (None) refuses the request.
fn selection_notify(
    time: u32,
    requestor: u32,
    selection: u32,
    target: u32,
    property: u32,
) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0] = 31;
    for (i, v) in [time, requestor, selection, target, property]
        .iter()
        .enumerate()
    {
        e[4 + 4 * i..8 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    e
}

/// An event this source acts on, decoded from its 32 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    ClientMessage {
        window: u32,
        type_: u32,
        data: [u32; 5],
    },
    SelectionRequest {
        time: u32,
        requestor: u32,
        selection: u32,
        target: u32,
        property: u32,
    },
    /// An error: its code, and the major opcode of the request it names.
    Error { code: u8, major: u8 },
    /// Anything else, by its code (the sent-event bit cleared).
    Other(u8),
}

fn decode(e: &[u8; 32]) -> Event {
    let at = |i| u32_at(e, i).unwrap_or(0);
    match e[0] & 0x7f {
        0 => Event::Error {
            code: e[1],
            major: e[10],
        },
        33 => Event::ClientMessage {
            window: at(4),
            type_: at(8),
            data: [at(12), at(16), at(20), at(24), at(28)],
        },
        30 => Event::SelectionRequest {
            time: at(4),
            requestor: at(12),
            selection: at(16),
            target: at(20),
            property: at(24),
        },
        code => Event::Other(code),
    }
}

/// One connection to the X server.
pub(crate) struct X {
    sock: UnixStream,
    /// The longest reply read: [`MAX_REPLY`], or more for `xgrab`'s image.
    pub(crate) max_reply: usize,
    /// The sequence number of the last request sent.
    seq: u16,
    setup: Setup,
    next_id: u32,
    /// Events read while a reply was awaited.
    queued: VecDeque<[u8; 32]>,
}

impl X {
    pub(crate) fn connect(display: &str) -> Result<X, Error> {
        let path = socket_path(display).ok_or_else(|| {
            Error::NoServer(format!("DISPLAY={display}: not a local display (:N)"))
        })?;
        let mut sock = UnixStream::connect(&path)
            .map_err(|e| Error::NoServer(format!("{}: {e}", path.display())))?;
        sock.set_read_timeout(Some(POLL))
            .map_err(|e| fault(e.to_string()))?;
        sock.write_all(&SETUP).map_err(|e| fault(e.to_string()))?;
        let mut x = X {
            sock,
            max_reply: MAX_REPLY,
            seq: 0,
            setup: Setup {
                id_base: 0,
                id_mask: 0,
                root: 0,
            },
            next_id: 0,
            queued: VecDeque::new(),
        };
        let deadline = Instant::now() + HANG;
        let mut head = [0u8; 8];
        x.fill(&mut head, deadline)?;
        let extra = usize::from(u16_at(&head, 6).unwrap_or(0)) * 4;
        if extra > MAX_REPLY {
            return Err(fault("a setup reply longer than this probe reads"));
        }
        let mut reply = head.to_vec();
        reply.resize(8 + extra, 0);
        x.fill(&mut reply[8..], deadline)?;
        x.setup = parse_setup(&reply).map_err(Error::NoServer)?;
        Ok(x)
    }

    /// Fill `buf` from the socket by `deadline`.
    fn fill(&mut self, buf: &mut [u8], deadline: Instant) -> Result<(), Error> {
        let mut got = 0;
        while got < buf.len() {
            match self.sock.read(&mut buf[got..]) {
                Ok(0) => return Err(fault("the X server closed the connection")),
                Ok(n) => got += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) =>
                {
                    if Instant::now() > deadline {
                        return Err(fault(format!(
                            "the X server sent nothing for {} s (a hang detector)",
                            HANG.as_secs()
                        )));
                    }
                }
                Err(e) => return Err(fault(e.to_string())),
            }
        }
        Ok(())
    }

    /// The next packet off the wire: an event or error (32 bytes), or a
    /// reply with its extra bytes.
    fn packet(&mut self, deadline: Instant) -> Result<Vec<u8>, Error> {
        let mut head = [0u8; 32];
        self.fill(&mut head, deadline)?;
        let mut p = head.to_vec();
        if head[0] == 1 {
            let extra = usize::try_from(u32_at(&head, 4).unwrap_or(0))
                .unwrap_or(usize::MAX)
                .saturating_mul(4);
            if extra > self.max_reply {
                return Err(fault("a reply longer than this probe reads"));
            }
            p.resize(32 + extra, 0);
            self.fill(&mut p[32..], deadline)?;
        }
        Ok(p)
    }

    fn send(&mut self, req: &[u8]) -> Result<u16, Error> {
        self.sock.write_all(req).map_err(|e| fault(e.to_string()))?;
        self.seq = self.seq.wrapping_add(1);
        Ok(self.seq)
    }

    /// Send a request that has a reply, and read the reply, keeping the
    /// events that come first.
    pub(crate) fn call(&mut self, req: &[u8]) -> Result<Vec<u8>, Error> {
        let seq = self.send(req)?;
        let deadline = Instant::now() + HANG;
        loop {
            let p = self.packet(deadline)?;
            let head: [u8; 32] = p
                .get(..32)
                .and_then(|h| h.try_into().ok())
                .ok_or_else(|| fault("a short packet"))?;
            match (p.first(), u16_at(&p, 2)) {
                (Some(1), Some(s)) if s == seq => return Ok(p),
                (Some(1), _) => return Err(fault("a reply to a request this probe did not make")),
                (Some(0), _) => return Err(fault(describe(&decode(&head), opcode_of(req)))),
                _ => self.queued.push_back(head),
            }
        }
    }

    /// The next event, the ones queued first.
    fn event(&mut self, deadline: Instant) -> Result<Event, Error> {
        if let Some(e) = self.queued.pop_front() {
            return Ok(decode(&e));
        }
        let p = self.packet(deadline)?;
        if p.first() == Some(&1) {
            return Err(fault("a reply nothing asked for"));
        }
        let head: [u8; 32] = p
            .get(..32)
            .and_then(|h| h.try_into().ok())
            .ok_or_else(|| fault("a short packet"))?;
        Ok(decode(&head))
    }

    fn new_id(&mut self) -> u32 {
        let mask = self.setup.id_mask;
        // The lowest set bit of the mask is the step between ids.
        let step = 1u32.checked_shl(mask.trailing_zeros()).unwrap_or(0);
        self.next_id = self.next_id.wrapping_add(step);
        self.setup.id_base | (self.next_id & mask)
    }

    fn intern(&mut self) -> Result<Atoms, Error> {
        let mut atoms = Vec::new();
        for name in NAMES {
            let r = self.call(&intern_atom(name))?;
            let atom = u32_at(&r, 8).ok_or_else(|| fault("a short InternAtom reply"))?;
            atoms.push((atom, name));
        }
        Ok(Atoms(atoms))
    }

    /// The first 32-bit value of `property` on `window`, if it has one.
    fn property_word(&mut self, window: u32, property: u32) -> Result<Option<(u32, u32)>, Error> {
        let r = self.call(&get_property(window, property))?;
        let format = r.get(1).copied().unwrap_or(0);
        let type_ = u32_at(&r, 8).unwrap_or(0);
        let len = u32_at(&r, 16).unwrap_or(0);
        if format != 32 || len == 0 {
            return Ok(None);
        }
        Ok(u32_at(&r, 32).map(|v| (type_, v)))
    }

    /// The centre of `window` in root coordinates.
    fn centre(&mut self, window: u32) -> Result<(i16, i16), Error> {
        let g = self.call(&get_geometry(window))?;
        let short = || fault("a short GetGeometry reply");
        let w = u16_at(&g, 16).ok_or_else(short)?;
        let h = u16_at(&g, 18).ok_or_else(short)?;
        let half = |v: u16| i16::try_from(v / 2).unwrap_or(i16::MAX);
        let root = self.setup.root;
        let t = self.call(&translate_coordinates(window, root, half(w), half(h)))?;
        let short = || fault("a short TranslateCoordinates reply");
        Ok((
            i16_at(&t, 12).ok_or_else(short)?,
            i16_at(&t, 14).ok_or_else(short)?,
        ))
    }
}

/// The request a request's bytes are, by its opcode, for an error's text.
fn opcode_of(req: &[u8]) -> u8 {
    req.first().copied().unwrap_or(0)
}

fn describe(e: &Event, sent: u8) -> String {
    match e {
        Event::Error { code, major } => {
            format!("the X server answered error {code} to request {major} (last sent {sent})")
        }
        other => format!("{other:?}"),
    }
}

/// An `XdndStatus` or `XdndFinished`, as the report prints it.
fn answer(a: &Atoms, accepted: bool, action: u32) -> String {
    format!("accept={} action={}", u8::from(accepted), a.name(action))
}

/// The drag: the connection, the atoms, the source and target windows, and
/// where the report goes.
struct Drag {
    x: X,
    a: Atoms,
    src: u32,
    target: u32,
    at: (i16, i16),
    uri: String,
    report: File,
    /// A data request held unanswered (the drop's first).
    held: Option<Event>,
}

impl Drag {
    fn say(&mut self, key: &str, value: &str) -> Result<(), Error> {
        println!("{key} {value}");
        writeln!(self.report, "{key} {value}")
            .and_then(|()| self.report.flush())
            .map_err(|e| fault(e.to_string()))
    }

    fn message(&mut self, type_: &str, data: [u32; 5]) -> Result<(), Error> {
        let t = self.a.id(type_);
        let e = client_message(self.target, t, data);
        self.x.send(&send_event(self.target, &e)).map(drop)
    }

    fn enter(&mut self) -> Result<(), Error> {
        let uri_list = self.a.id("text/uri-list");
        self.message("XdndEnter", [self.src, VERSION << 24, uri_list, 0, 0])
    }

    fn position(&mut self) -> Result<(), Error> {
        let (x, y) = self.at;
        let packed = (u32::from(x.cast_unsigned()) << 16) | u32::from(y.cast_unsigned());
        let copy = self.a.id("XdndActionCopy");
        self.message("XdndPosition", [self.src, 0, packed, 0, copy])
    }

    /// Answer a data request: the uri as `text/uri-list`, anything else
    /// refused. What was asked, as the report prints it.
    fn serve(&mut self, request: &Event) -> Result<String, Error> {
        let Event::SelectionRequest {
            time,
            requestor,
            selection,
            target,
            property,
        } = *request
        else {
            return Ok("nothing".into());
        };
        let uri_list = self.a.id("text/uri-list");
        let property = if property == 0 { target } else { property };
        let asked = format!("{} into {}", self.a.name(target), self.a.name(property));
        let given = if target == uri_list {
            let data = format!("{}\r\n", self.uri);
            self.x.send(&change_property(
                requestor,
                property,
                uri_list,
                data.as_bytes(),
            ))?;
            property
        } else {
            0
        };
        let notify = selection_notify(time, requestor, selection, target, given);
        self.x.send(&send_event(requestor, &notify))?;
        Ok(if given == 0 {
            format!("{asked}, refused")
        } else {
            asked
        })
    }

    /// Wait for the target's next `XdndStatus`, answering a data request
    /// that comes first (or holding it, when `hold`). The status, and what
    /// was asked meanwhile.
    fn status(&mut self, hold: bool) -> Result<(String, Option<String>), Error> {
        let deadline = Instant::now() + HANG;
        let status = self.a.id("XdndStatus");
        let selection = self.a.id("XdndSelection");
        let mut asked = None;
        loop {
            let ev = self.x.event(deadline)?;
            match &ev {
                Event::ClientMessage {
                    window,
                    type_,
                    data,
                } if *window == self.src && *type_ == status => {
                    return Ok((answer(&self.a, data[1] & 1 == 1, data[4]), asked));
                }
                Event::SelectionRequest {
                    selection: s,
                    target,
                    ..
                } if *s == selection => {
                    if hold {
                        asked = Some(format!("{} (held)", self.a.name(*target)));
                        self.held = Some(ev);
                    } else {
                        asked = Some(self.serve(&ev)?);
                    }
                }
                Event::Error { .. } => return Err(fault(describe(&ev, 0))),
                _ => {}
            }
        }
    }

    /// Wait for the target's `XdndFinished`.
    fn finished(&mut self) -> Result<String, Error> {
        let deadline = Instant::now() + HANG;
        let finished = self.a.id("XdndFinished");
        loop {
            let ev = self.x.event(deadline)?;
            match &ev {
                Event::ClientMessage {
                    window,
                    type_,
                    data,
                } if *window == self.src && *type_ == finished => {
                    return Ok(answer(&self.a, data[1] & 1 == 1, data[2]));
                }
                Event::Error { .. } => return Err(fault(describe(&ev, 0))),
                _ => {}
            }
        }
    }
}

/// Create `<marks>/<name>`, then wait for `<marks>/<name>.seen`.
fn hand_over(marks: &Path, name: &str) -> Result<(), Error> {
    File::create(marks.join(name)).map_err(|e| fault(format!("{name}: {e}")))?;
    let seen = marks.join(format!("{name}.seen"));
    let t0 = Instant::now();
    while !seen.exists() {
        if t0.elapsed() > LAB {
            return Err(fault(format!(
                "{} never appeared ({} s)",
                seen.display(),
                LAB.as_secs()
            )));
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}

/// `xdnd <window> <uri> <marks>`.
pub(crate) fn run(target: u32, uri: &str, marks: &Path) -> Result<(), Error> {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let mut x = X::connect(&display)?;
    let a = x.intern()?;
    let src = x.new_id();
    let root = x.setup.root;
    x.send(&create_window(src, root))?;
    let report = File::create(marks.join("report"))
        .map_err(|e| fault(format!("{}: {e}", marks.display())))?;
    let aware = match x.property_word(target, a.id("XdndAware"))? {
        Some((ATOM_ATOM, version)) => version.to_string(),
        Some((type_, _)) => format!("not an ATOM ({})", a.name(type_)),
        None => "none".into(),
    };
    let at = x.centre(target)?;
    x.send(&set_selection_owner(src, a.id("XdndSelection")))?;
    let mut d = Drag {
        x,
        a,
        src,
        target,
        at,
        uri: uri.to_string(),
        report,
        held: None,
    };
    d.say("aware", &aware)?;
    // A hover that leaves.
    d.enter()?;
    d.position()?;
    let (status, asked) = d.status(false)?;
    d.say("hover.status", &status)?;
    d.say("hover.asked", asked.as_deref().unwrap_or("nothing"))?;
    d.position()?;
    let (status, asked) = d.status(false)?;
    d.say("hover.status.2", &status)?;
    if let Some(asked) = asked {
        d.say("hover.asked.2", &asked)?;
    }
    hand_over(marks, "hovering")?;
    d.message("XdndLeave", [src, 0, 0, 0, 0])?;
    // A hover that drops: the first status says the leave was handled, and
    // the data stays unanswered until the lab has seen the window then.
    d.enter()?;
    d.position()?;
    let (status, asked) = d.status(true)?;
    d.say("drop.status", &status)?;
    d.say("drop.asked", asked.as_deref().unwrap_or("nothing"))?;
    hand_over(marks, "left")?;
    if let Some(request) = d.held.take() {
        d.serve(&request)?;
    }
    d.position()?;
    let (status, asked) = d.status(false)?;
    d.say("drop.status.2", &status)?;
    if let Some(asked) = asked {
        d.say("drop.asked.2", &asked)?;
    }
    d.message("XdndDrop", [src, 0, 0, 0, 0])?;
    let done = d.finished()?;
    d.say("drop.finished", &done)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_display_names_its_socket() {
        assert_eq!(
            socket_path(":91"),
            Some(PathBuf::from("/tmp/.X11-unix/X91"))
        );
        assert_eq!(
            socket_path(":92.0"),
            Some(PathBuf::from("/tmp/.X11-unix/X92"))
        );
        assert_eq!(socket_path("host:0"), None);
        assert_eq!(socket_path(":"), None);
    }

    /// Requests as the X11R7.7 protocol encodes them (little-endian).
    #[test]
    fn requests_are_encoded_as_the_protocol_lays_them_out() {
        assert_eq!(
            intern_atom("XdndAware"),
            [
                16, 0, 5, 0, 9, 0, 0, 0, b'X', b'd', b'n', b'd', b'A', b'w', b'a', b'r', b'e', 0,
                0, 0
            ]
        );
        let cw = create_window(0x0020_0001, 0x0000_0505);
        assert_eq!(cw.len(), 32);
        assert_eq!(&cw[..4], [1, 0, 8, 0]);
        assert_eq!(u32_at(&cw, 4), Some(0x0020_0001));
        assert_eq!(u32_at(&cw, 8), Some(0x0505));
        assert_eq!(u16_at(&cw, 16), Some(1), "width");
        assert_eq!(u16_at(&cw, 22), Some(2), "InputOnly");
        let cp = change_property(7, 8, 9, b"file:///a\r\n");
        assert_eq!(&cp[..4], [18, 0, 9, 0]);
        assert_eq!(cp[16], 8, "format 8");
        assert_eq!(u32_at(&cp, 20), Some(11), "length in bytes");
        assert_eq!(&cp[24..35], b"file:///a\r\n");
        assert_eq!(cp.len(), 36, "padded to a word");
        let se = send_event(3, &client_message(3, 4, [1, 2, 3, 4, 5]));
        assert_eq!(&se[..4], [25, 0, 11, 0]);
        assert_eq!(se.len(), 44);
        assert_eq!(se[12], 33, "a ClientMessage");
        assert_eq!(se[13], 32, "format 32");
        assert_eq!(u32_at(&se, 12 + 12), Some(1));
        assert_eq!(u32_at(&se, 12 + 28), Some(5));
        assert_eq!(get_property(1, 2).len(), 24);
        assert_eq!(set_selection_owner(1, 2)[..4], [22, 0, 4, 0]);
        assert_eq!(get_geometry(1)[..4], [14, 0, 2, 0]);
        let tc = translate_coordinates(1, 2, -3, 4);
        assert_eq!(&tc[..4], [40, 0, 4, 0]);
        assert_eq!(i16_at(&tc, 12), Some(-3));
    }

    #[test]
    fn the_setup_reply_names_the_root_and_the_ids() {
        let mut r = vec![1, 0, 11, 0, 0, 0, 0, 0];
        r.extend_from_slice(&words(&[0, 0x0020_0000, 0x001f_ffff, 0]));
        r.extend_from_slice(&5u16.to_le_bytes()); // vendor length
        // maximum-request-length, one screen, two formats, six bytes of image
        // facts, four unused.
        r.extend_from_slice(&[0, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        r.extend_from_slice(b"Xorg\0\0\0\0"); // vendor, padded to 8
        r.extend_from_slice(&[0; 16]); // two formats
        r.extend_from_slice(&0x0000_0505u32.to_le_bytes());
        assert_eq!(
            parse_setup(&r),
            Ok(Setup {
                id_base: 0x0020_0000,
                id_mask: 0x001f_ffff,
                root: 0x0505
            })
        );
        let mut refused = vec![0, 6, 11, 0, 0, 0, 2, 0];
        refused.extend_from_slice(b"no way\0\0");
        assert!(
            parse_setup(&refused)
                .expect_err("refused")
                .ends_with("no way")
        );
        assert!(parse_setup(&r[..30]).is_err(), "short");
        assert!(parse_setup(&[]).is_err());
    }

    #[test]
    fn events_decode_by_their_code() {
        let cm = client_message(9, 10, [11, 1, 0, 0, 12]);
        assert_eq!(
            decode(&cm),
            Event::ClientMessage {
                window: 9,
                type_: 10,
                data: [11, 1, 0, 0, 12]
            }
        );
        let mut sent = selection_notify(1, 2, 3, 4, 5);
        sent[0] |= 0x80;
        assert_eq!(
            decode(&sent),
            Event::Other(31),
            "the sent-event bit is cleared"
        );
        let mut req = [0u8; 32];
        req[0] = 30;
        for (i, v) in [7u32, 8, 9, 10, 11, 12].iter().enumerate() {
            req[4 + 4 * i..8 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        assert_eq!(
            decode(&req),
            Event::SelectionRequest {
                time: 7,
                requestor: 9,
                selection: 10,
                target: 11,
                property: 12
            }
        );
        let mut err = [0u8; 32];
        err[1] = 3;
        err[10] = 25;
        assert_eq!(decode(&err), Event::Error { code: 3, major: 25 });
    }
}
