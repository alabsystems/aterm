// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The platform I/O a program's output asks aterm for — an OSC 52 query's
//! pasteboard read, a Kitty `t=f`/`t=t`/`t=s` transmission's file or shared
//! memory read — done by the session's PTY reader BEFORE it takes the terminal
//! lock to feed those bytes, never inside `Terminal::process()`.
//!
//! WHY (2026-10-14): the engine answers both through host callbacks it runs
//! inside `process()`, and the reader runs `process()` under the terminal lock.
//! A pasteboard read blocks while a Universal Clipboard item is fetched from
//! another device or a lazy provider is busy; a file read blocks while an
//! evicted iCloud file downloads or a stalled SMB/NFS volume runs out its
//! timeout. For all that time the session's lock was held, and the redraw,
//! which takes the front terminal's lock, stopped every window. Both are opt-in
//! (`allow_osc52_query`, `allow_kitty_file_transfer`) and stay so.
//!
//! HOW: the reader hands each slice it is about to feed to [`Prefetch::reads`]
//! first. It scans the slice (with the unfinished tail of the one before) for
//! complete OSC 52 queries and Kitty transmissions that read a non-direct
//! medium, does each read the session admits ([`Flags`], read under the
//! reader's previous hold), and keeps the results on the reader's thread until
//! the slice has been fed. The callbacks, which run on that thread inside
//! `process()`, only take a result ([`take_clipboard_query`],
//! [`take_kitty_medium`]), so the engine answers exactly as before, in byte
//! order, with nothing deferred. A read the scan did not do — a terminator it
//! does not read (an 8-bit ST in C1 mode), a file named across chunks, the
//! slice right after a session first admits a query — is answered as a failed
//! read is (no reply, a refused image), and is never done under the lock.
//!
//! What a slice's reads hold is bounded, as it was when the engine read one
//! medium at a time: the pasteboard is read once per slice, whatever number of
//! queries share it, and the media reads stop at the command that finds
//! [`READ_BUDGET`] already held. The slice is cut there ([`Burst::covered`]),
//! and the rest is the next slice's, read once this one is fed and its media
//! dropped. Before (2026-10-14 review), one 64 KiB burst of 2048 short `t=f`
//! commands held 2048 media at once, up to 8 GiB.

use std::cell::RefCell;

use aterm_core::terminal::kitty_graphics::{KittyAction, KittyMedium, parse_kitty_command};
use aterm_core::terminal::{ClipboardSelection, Terminal};

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
/// The 8-bit string terminator, which ends an APC whatever the C1 mode.
const ST8: u8 = 0x9c;

/// The longest unfinished candidate carried from one slice to the next: a
/// Kitty command naming a file is a few hundred bytes, an OSC 52 query a dozen.
const CARRY_MAX: usize = 16 * 1024;

/// What the media one slice's reads hold may reach before they stop: one
/// largest medium (`spawn::MAX_KITTY_MEDIUM_BYTES`). The read that reaches it
/// is the slice's last, so a slice never holds two largest media's worth.
const READ_BUDGET: usize = crate::spawn::MAX_KITTY_MEDIUM_BYTES as usize;

/// What one session admits, read by its reader under each term-lock hold for
/// the slice after it ([`Flags::of`]). With neither, a slice is not scanned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Flags {
    clipboard_query: bool,
    kitty_media: bool,
}

impl Flags {
    /// The session's flags now. On Linux a query is answered only from aterm's
    /// own selection, a read that never blocks (`spawn::install_clipboard`), so
    /// there is nothing to fetch for it.
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

/// What this thread fetched for the slice it is feeding, and the unfinished
/// candidate the last slice ended in.
#[derive(Default)]
struct Fetched {
    /// The pasteboard as read for this slice's queries, and how many of them
    /// have yet to take it.
    clipboard: Option<String>,
    queries: usize,
    kitty: Vec<(KittyMedium, String, Option<Vec<u8>>)>,
    carry: Vec<u8>,
}

thread_local! {
    static FETCHED: RefCell<Fetched> = RefCell::new(Fetched::default());
}

/// A PTY reader's side of the prefetch: what its session admits, read under
/// each hold for the slice after it, and once, briefly, before the first.
#[derive(Default)]
pub(crate) struct Prefetch {
    flags: Option<Flags>,
}

impl Prefetch {
    /// Do the reads `slice` asks for that the session admits, before the hold
    /// that feeds it. The hold feeds `slice[..covered]` ([`Burst::covered`]);
    /// the rest is the next slice.
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

/// The reads one slice asked for, held until it has been fed: dropping it
/// discards whatever the engine did not take.
pub(crate) struct Burst {
    covered: usize,
}

impl Burst {
    /// Do the reads `slice` asks for that `flags` admits, before any term-lock
    /// hold, through the platform: the pasteboard (`control::pbpaste`, an empty
    /// one read as an empty answer) and the Kitty medium readers
    /// (`spawn::read_kitty_medium`).
    fn begin(slice: &[u8], flags: Flags) -> Self {
        Self::begin_with(
            slice,
            flags,
            || crate::control::pbpaste().unwrap_or_default(),
            crate::spawn::read_kitty_medium,
        )
    }

    /// [`Self::begin`] with the readers as parameters.
    fn begin_with(
        slice: &[u8],
        flags: Flags,
        mut pasteboard: impl FnMut() -> String,
        mut medium: impl FnMut(KittyMedium, &str) -> Option<Vec<u8>>,
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
                // The slice ends before the command the budget stopped at. A
                // read came before it, so it begins at or past the carry's end
                // (the carry holds one escape's start and none of its end);
                // where it begins right there, the slice takes its ESC and
                // carries it, so every slice feeds at least one byte.
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
        Self { covered }
    }

    /// How much of the slice its reads cover, from its start: the bytes the
    /// hold feeds. All of it, unless the media reads stopped at
    /// [`READ_BUDGET`].
    pub(crate) const fn covered(&self) -> usize {
        self.covered
    }
}

impl Drop for Burst {
    fn drop(&mut self) {
        FETCHED.with(|cell| {
            if let Ok(mut fetched) = cell.try_borrow_mut() {
                fetched.clipboard = None;
                fetched.queries = 0;
                fetched.kitty.clear();
            }
        });
    }
}

/// The OSC 52 query callback's answer: the pasteboard as read for the slice
/// being fed on this thread, for as many queries as the scan found in it, or
/// `None` (no reply) when none was read.
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

/// The Kitty medium resolver: the bytes read for `(medium, name)` in the slice
/// being fed on this thread, or `None` (the image refused) when none were.
pub(crate) fn take_kitty_medium(medium: KittyMedium, name: &str) -> Option<Vec<u8>> {
    FETCHED.with(|cell| {
        let mut fetched = cell.try_borrow_mut().ok()?;
        let at = fetched
            .kitty
            .iter()
            .position(|(kind, fetched_name, _)| *kind == medium && fetched_name == name)?;
        fetched.kitty.remove(at).2
    })
}

/// Every complete request in `region`, in order, with where its escape
/// begins, and where an unfinished one that could still become a request
/// begins.
///
/// It reads the strings as the parser does: `ESC ]` opens an OSC, which BEL or
/// ESC ends and dispatches; `ESC _` opens an APC, which ESC or the 8-bit ST
/// ends and dispatches; CAN and SUB cancel either. An ESC always ends the
/// string it falls in and starts an escape.
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
        // Resume at the terminator: an ESC there begins the next escape.
        i = body_at + len + usize::from(body_rest[len] != ESC);
    }
    (requests, None)
}

/// A whole OSC body that the engine answers as a clipboard query: `52`, a
/// selection list naming at least one selection (or none, which is the
/// clipboard), and `?` as its data.
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

/// Whether an OSC body cut off by the end of a burst could still become an
/// OSC 52 query.
fn could_be_osc52_query(body: &[u8]) -> bool {
    let mut fields = body.split(|&b| b == b';');
    let command = fields.next().unwrap_or_default();
    match (fields.next(), fields.next()) {
        (None, _) => b"52".starts_with(command),
        (Some(_), None) => command == b"52",
        (Some(_), Some(data)) => command == b"52" && b"?".starts_with(data),
    }
}

/// A whole APC body the engine resolves through the host: a Kitty transmit or
/// frame (whose data it reads) on a non-direct medium, in one command.
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
mod tests {
    use super::*;

    /// The reads one slice makes through counting readers, and what the
    /// engine can take of them: (pasteboard reads, queries answered, medium
    /// reads by name).
    fn reads(slice: &[u8], flags: Flags) -> (usize, usize, Vec<String>) {
        let mut pasteboard = 0;
        let mut media = Vec::new();
        let _burst = Burst::begin_with(
            slice,
            flags,
            || {
                pasteboard += 1;
                "board".to_string()
            },
            |_, name| {
                media.push(name.to_string());
                Some(vec![1])
            },
        );
        let answered = std::iter::from_fn(take_clipboard_query)
            .inspect(|board| assert_eq!(board, "board"))
            .count();
        (pasteboard, answered, media)
    }

    const ALL: Flags = Flags {
        clipboard_query: true,
        kitty_media: true,
    };

    /// A Kitty command naming `path` (base64 in its payload, as the protocol
    /// carries it), ST-terminated.
    fn file(control: &str, path: &str) -> Vec<u8> {
        let mut apc = format!("\x1b_G{control};").into_bytes();
        apc.extend_from_slice(
            aterm_codec::base64::encode(path.as_bytes())
                .unwrap()
                .as_bytes(),
        );
        apc.extend_from_slice(b"\x1b\\");
        apc
    }

    /// A slice is read for exactly the requests the engine sends the host — an
    /// OSC 52 query (BEL- or ST-terminated), the pasteboard once for all of a
    /// slice's queries, and a Kitty transmit naming a file, in one piece or cut
    /// across two slices — and for nothing else: an OSC 52 set, a canceled
    /// query, a Kitty query or chunked load, or anything a session does not
    /// admit.
    #[test]
    fn a_burst_is_read_for_the_requests_the_engine_will_make() {
        let mut burst = b"text\x1b]52;c;?\x07more\x1b]52;p;?\x1b\\".to_vec();
        burst.extend_from_slice(&file("a=T,f=100,t=f", "/tmp/a.png"));
        assert_eq!(reads(&burst, ALL), (1, 2, vec!["/tmp/a.png".to_string()]));

        let mut ignored = b"\x1b]52;c;cGxhbnRlZA==\x07\x1b]52;c;?\x18\x1b]0;?\x07".to_vec();
        ignored.extend_from_slice(&file("a=q,t=f", "/tmp/q.png"));
        ignored.extend_from_slice(&file("a=T,t=f,m=1", "/tmp/chunk"));
        ignored.extend_from_slice(&file("a=T,f=100", "direct"));
        assert_eq!(reads(&ignored, ALL), (0, 0, vec![]));
        assert_eq!(reads(&burst, Flags::default()), (0, 0, vec![]));

        // Cut anywhere, the second half completes what the first began.
        for cut in 1..burst.len() {
            let (head, tail) = burst.split_at(cut);
            let first = reads(head, ALL);
            let second = reads(tail, ALL);
            assert_eq!(
                (first.1 + second.1, [first.2, second.2].concat()),
                (2, vec!["/tmp/a.png".to_string()]),
                "cut at {cut}"
            );
        }
    }

    /// The largest medium a read returns.
    const LARGEST: usize = crate::spawn::MAX_KITTY_MEDIUM_BYTES as usize;

    /// Feed `parts` (the PTY's reads) slice by slice as the reader does, each
    /// medium read returning [`LARGEST`] bytes: the names read, in order, and
    /// the most one slice's reads held.
    fn feed_largest(parts: &[&[u8]]) -> (Vec<String>, usize) {
        let mut names = Vec::new();
        let mut most = 0;
        for part in parts {
            let mut off = 0;
            while off < part.len() {
                let mut held = 0;
                let fetched = Burst::begin_with(&part[off..], ALL, String::new, |_, name| {
                    names.push(name.to_string());
                    held += LARGEST;
                    Some(vec![0; LARGEST])
                });
                most = most.max(held);
                assert!(fetched.covered() > 0, "a slice that feeds nothing");
                off += fetched.covered();
            }
        }
        (names, most)
    }

    /// One burst of many short `t=f` commands, each naming a file as large as
    /// a medium may be, is read slice by slice: no slice holds more than two
    /// largest media, and every command is still read exactly once, in order,
    /// however the PTY's reads cut the burst — including right before a
    /// command's ESC that also begins the next. Before (2026-10-14 review), a
    /// burst's reads were all held at once: a 64 KiB burst of 2048 such
    /// commands held 8 GiB.
    #[test]
    fn a_slice_holds_at_most_two_largest_media_and_reads_each_once() {
        let names: Vec<String> = (0..48).map(|n| format!("/m/{n}")).collect();
        let burst: Vec<u8> = names
            .iter()
            .flat_map(|name| file("a=t,f=32,s=10,v=20,t=f", name))
            .collect();
        let (read, most) = feed_largest(&[&burst]);
        assert_eq!(read, names);
        assert!(most <= 2 * LARGEST, "one slice held {most} bytes of media");

        // Commands each ended by the next one's ESC, cut at every byte.
        let short = &names[..4];
        let mut adjacent: Vec<u8> = short
            .iter()
            .flat_map(|name| {
                let mut apc = file("a=t,f=32,s=10,v=20,t=f", name);
                apc.truncate(apc.len() - 2);
                apc
            })
            .collect();
        adjacent.extend_from_slice(b"\x1b\\");
        for cut in 1..adjacent.len() {
            let (head, tail) = adjacent.split_at(cut);
            let (read, most) = feed_largest(&[head, tail]);
            assert_eq!(read, short, "cut at {cut}");
            assert!(most <= 2 * LARGEST, "cut at {cut}: {most}");
        }
    }
}
