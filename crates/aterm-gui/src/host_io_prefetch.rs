// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unlocked PTY-reader prefetch for OSC 52 queries and Kitty file/shm transfers
//! ([`Prefetch::reads`], [`Flags`], [`take_clipboard_query`], [`take_kitty_medium`],
//! [`READ_BUDGET`], [`Burst::covered`]).

use std::cell::RefCell;

use aterm_core::terminal::kitty_graphics::{KittyAction, KittyMedium, parse_kitty_command};
use aterm_core::terminal::{ClipboardSelection, Terminal};

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
/// The 8-bit string terminator, which ends an APC whatever the C1 mode.
const ST8: u8 = 0x9c;

/// The longest unfinished candidate carried from one slice to the next.
const CARRY_MAX: usize = 16 * 1024;

/// What the media one slice's reads hold may reach before they stop.
const READ_BUDGET: usize = crate::spawn::MAX_KITTY_MEDIUM_BYTES as usize;

/// What one session admits, updated under each term-lock hold ([`Flags::of`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Flags {
    clipboard_query: bool,
    kitty_media: bool,
}

impl Flags {
    /// The session's flags now.
    fn of(t: &Terminal) -> Self {
        Self {
            clipboard_query: cfg!(not(target_os = "linux")) && t.clipboard_query_admitted(),
            kitty_media: t.kitty_file_resolver_installed(),
        }
    }

    const fn any(self) -> bool {
        self.clipboard_query || self.kitty_media
    }
}

/// One request a scan found, in byte order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Request {
    ClipboardQuery,
    Kitty(KittyMedium, String),
}

/// What this thread fetched for the slice it is feeding.
#[derive(Default)]
struct Fetched {
    /// The pasteboard as read for this slice's queries, and how many of them have yet to take it.
    clipboard: Option<String>,
    queries: usize,
    kitty: Vec<(KittyMedium, String, Option<Vec<u8>>)>,
    /// Media taken by [`take_kitty_medium`], consumed when [`Burst`] drops.
    taken: Vec<(KittyMedium, String, bool)>,
    carry: Vec<u8>,
}

thread_local! {
    static FETCHED: RefCell<Fetched> = RefCell::new(Fetched::default());
}

/// A PTY reader's side of the prefetch.
#[derive(Default)]
pub(crate) struct Prefetch {
    flags: Option<Flags>,
}

impl Prefetch {
    /// Prefetch the reads `slice` asks for that the session admits ([`Burst::covered`]).
    pub(crate) fn reads(&mut self, term: &std::sync::Mutex<Terminal>, slice: &[u8]) -> Burst {
        let flags = *self
            .flags
            .get_or_insert_with(|| Flags::of(&crate::term_lock(term)));
        Burst::begin(slice, flags)
    }

    /// Note what the session admits now, under the hold that fed a slice.
    pub(crate) fn held(&mut self, term: &Terminal) {
        self.flags = Some(Flags::of(term));
    }
}

/// The reads one slice asked for, held until it has been fed.
pub(crate) struct Burst {
    covered: usize,
    consume: fn(KittyMedium, &str, bool),
}

impl Burst {
    /// Do the reads `slice` asks for that `flags` admits.
    fn begin(slice: &[u8], flags: Flags) -> Self {
        Self::begin_with(
            slice,
            flags,
            || crate::control::pbpaste().unwrap_or_default(),
            crate::spawn::read_kitty_medium,
            crate::spawn::consume_kitty_medium,
        )
    }

    /// [`Self::begin`] with the readers, and what consumes a taken medium, as parameters.
    fn begin_with(
        slice: &[u8],
        flags: Flags,
        mut pasteboard: impl FnMut() -> String,
        mut medium: impl FnMut(KittyMedium, &str) -> Option<Vec<u8>>,
        consume: fn(KittyMedium, &str, bool),
    ) -> Self {
        let covered = FETCHED.with(|cell| {
            let Ok(mut fetched) = cell.try_borrow_mut() else {
                return slice.len();
            };
            if !flags.any() {
                fetched.carry.clear();
                return slice.len();
            }
            let carried = fetched.carry.len();
            let joined;
            let region = if carried == 0 {
                slice
            } else {
                let mut carry = std::mem::take(&mut fetched.carry);
                carry.extend_from_slice(slice);
                joined = carry;
                &joined[..]
            };
            let (requests, unfinished) = scan(region);
            let mut held = 0usize;
            let mut cut = None;
            for (at, request) in requests {
                match request {
                    Request::ClipboardQuery if flags.clipboard_query => {
                        if fetched.clipboard.is_none() {
                            fetched.clipboard = Some(pasteboard());
                        }
                        fetched.queries += 1;
                    }
                    Request::Kitty(kind, name) if flags.kitty_media => {
                        if held >= READ_BUDGET {
                            cut = Some(at);
                            break;
                        }
                        let bytes = medium(kind, &name);
                        held = held.saturating_add(bytes.as_ref().map_or(0, Vec::len));
                        fetched.kitty.push((kind, name, bytes));
                    }
                    _ => {}
                }
            }
            match cut {
                Some(at) => {
                    let end = at.max(carried + 1);
                    fetched.carry = region[at..end].to_vec();
                    end - carried
                }
                None => {
                    fetched.carry = unfinished
                        .map(|at| &region[at..])
                        .filter(|tail| tail.len() <= CARRY_MAX)
                        .map(<[u8]>::to_vec)
                        .unwrap_or_default();
                    slice.len()
                }
            }
        });
        Self { covered, consume }
    }

    /// Prefix length of the slice covered by its reads ([`READ_BUDGET`]).
    pub(crate) const fn covered(&self) -> usize {
        self.covered
    }
}

impl Drop for Burst {
    fn drop(&mut self) {
        let taken = FETCHED.with(|cell| {
            let Ok(mut fetched) = cell.try_borrow_mut() else {
                return Vec::new();
            };
            fetched.clipboard = None;
            fetched.queries = 0;
            fetched.kitty.clear();
            std::mem::take(&mut fetched.taken)
        });
        for (medium, name, read) in taken {
            (self.consume)(medium, &name, read);
        }
    }
}

/// The OSC 52 query callback's answer.
#[cfg(any(not(target_os = "linux"), test))]
pub(crate) fn take_clipboard_query() -> Option<String> {
    FETCHED.with(|cell| {
        let mut fetched = cell.try_borrow_mut().ok()?;
        fetched.queries = fetched.queries.checked_sub(1)?;
        if fetched.queries == 0 {
            fetched.clipboard.take()
        } else {
            fetched.clipboard.clone()
        }
    })
}

/// Take the prefetched Kitty medium `(medium, name)` for the slice being fed ([`Burst`]).
pub(crate) fn take_kitty_medium(medium: KittyMedium, name: &str) -> Option<Vec<u8>> {
    FETCHED.with(|cell| {
        let mut fetched = cell.try_borrow_mut().ok()?;
        let at = fetched
            .kitty
            .iter()
            .position(|(kind, fetched_name, _)| *kind == medium && fetched_name == name)?;
        let (kind, name, bytes) = fetched.kitty.remove(at);
        fetched.taken.push((kind, name, bytes.is_some()));
        bytes
    })
}

/// Every complete request in `region`.
fn scan(region: &[u8]) -> (Vec<(usize, Request)>, Option<usize>) {
    let mut requests = Vec::new();
    let mut i = 0;
    while let Some(found) = region
        .get(i..)
        .and_then(|rest| rest.iter().position(|&b| b == ESC))
    {
        let at = i + found;
        let Some(&kind) = region.get(at + 1) else {
            return (requests, Some(at));
        };
        let osc = match kind {
            b']' => true,
            b'_' => false,
            _ => {
                i = at + 1;
                continue;
            }
        };
        let body_at = at + 2;
        let body_rest = region.get(body_at..).unwrap_or_default();
        let end = body_rest.iter().position(|&b| {
            b == ESC || b == CAN || b == SUB || if osc { b == BEL } else { b == ST8 }
        });
        let Some(len) = end else {
            let ours = if osc {
                could_be_osc52_query(body_rest)
            } else {
                body_rest.first().is_none_or(|&b| b == b'G')
            };
            return (requests, ours.then_some(at));
        };
        let body = &body_rest[..len];
        let canceled = matches!(body_rest[len], CAN | SUB);
        if !canceled {
            if osc {
                if is_osc52_query(body) {
                    requests.push((at, Request::ClipboardQuery));
                }
            } else if let Some(request) = kitty_medium_request(body) {
                requests.push((at, request));
            }
        }
        i = body_at + len + usize::from(body_rest[len] != ESC);
    }
    (requests, None)
}

/// A whole OSC body that the engine answers as a clipboard query.
fn is_osc52_query(body: &[u8]) -> bool {
    let mut fields = body.split(|&b| b == b';');
    fields.next() == Some(b"52")
        && fields.next().is_some_and(|selections| {
            selections.is_empty()
                || selections
                    .iter()
                    .any(|&c| ClipboardSelection::from_char(char::from(c)).is_some())
        })
        && fields.next() == Some(b"?")
}

/// Whether an OSC body cut off by the end of a burst could still become an OSC 52 query.
fn could_be_osc52_query(body: &[u8]) -> bool {
    let mut fields = body.split(|&b| b == b';');
    let command = fields.next().unwrap_or_default();
    match (fields.next(), fields.next()) {
        (None, _) => b"52".starts_with(command),
        (Some(_), None) => command == b"52",
        (Some(_), Some(data)) => command == b"52" && b"?".starts_with(data),
    }
}

/// A whole APC body the engine resolves through the host.
fn kitty_medium_request(body: &[u8]) -> Option<Request> {
    let command = parse_kitty_command(body)?;
    let reads = matches!(
        command.action,
        KittyAction::Transmit | KittyAction::TransmitAndDisplay | KittyAction::Frame
    );
    if !reads || command.more || command.medium == KittyMedium::Direct {
        return None;
    }
    let name = String::from_utf8(command.payload).ok()?;
    (!name.is_empty()).then_some(Request::Kitty(command.medium, name))
}

/// Feed `bytes` to `term` as the PTY reader feeds a burst: slice by slice,
/// each slice's reads first, with the terminal unlocked, then the hold that
/// processes what they cover.
#[cfg(test)]
pub(crate) fn feed_as_reader(term: &std::sync::Mutex<Terminal>, bytes: &[u8]) {
    let mut prefetch = Prefetch::default();
    let mut off = 0;
    while off < bytes.len() {
        let fetched = prefetch.reads(term, &bytes[off..]);
        let end = off + fetched.covered();
        let mut t = crate::term_lock(term);
        t.process(&bytes[off..end]);
        prefetch.held(&t);
        off = end;
    }
}

#[cfg(test)]
#[path = "host_io_prefetch_tests.rs"]
mod tests;
