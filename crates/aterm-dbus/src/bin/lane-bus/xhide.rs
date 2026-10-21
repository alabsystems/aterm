// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-bus xhide <display> <server> <extension>[,<extension>...]|-`: an X
//! server that lacks the named extensions, made of one that has them. A
//! proxy listening as X display `<display>` (`/tmp/.X11-unix/X<n>`) carries
//! each client to X display `<server>` ([`crate::relay`], descriptors
//! included) and answers every `QueryExtension` of a named extension as the
//! X Window System Protocol answers it for one the server does not have:
//! `present` false, no opcode, event or error base. `-` hides nothing (the
//! lane's control: the proxy alone changes nothing a client sees). The
//! token `XInput2` hides XInput 2 alone, as an XInput 1 server lacks it:
//! XInputExtension stays present, and each `XIQueryVersion` (its request
//! 47) is answered with the core `Request` error, which a server whose
//! XInputExtension predates version 2 answers for a request it does not
//! know.
//!
//! Xvfb can turn off RANDR (`-extension RANDR`) but not XInputExtension or
//! XKEYBOARD, which Xorg's runtime-disable list leaves out, so the lane
//! drives aterm's refusal of a server without XInput 2 or XKB through this
//! (docs/AUDIT-third-party-2026-10-15.md, L-HEADER-LW7). Little-endian
//! clients only (the lane's); a big-endian one's setup is refused.
//!
//! The core protocol, from the X Window System Protocol (X11R7.7): a
//! client's setup request is 12 bytes (byte order, version, the
//! authorization's name and data lengths) and the two padded strings; each
//! request then carries its length in 4-byte units at bytes 2-3, or, as 0,
//! a 32-bit length after the header (BIG-REQUESTS), and the server numbers
//! requests from 1 in the 16 bits each reply, error and event repeats at
//! bytes 2-3. The server's setup answer is 8 bytes and 4 times the length at
//! bytes 6-7; then every error and event is 32 bytes, and a reply (type 1)
//! and a generic event (type 35, XGE) 32 plus 4 times the length at bytes
//! 4-7. `QueryExtension` is request 98, its name's length at bytes 4-5 and
//! the name from byte 8; its reply's `present`, major opcode, first event
//! and first error are bytes 8-11. An extension's request carries its major
//! opcode in byte 0 and its minor in byte 1. An error is type 0, its code in
//! byte 1, the sequence at bytes 2-3, the bad value at 4-7, the minor opcode
//! at 8-9 and the major at byte 10.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::relay::{self, Filter};

/// `QueryExtension`'s request number.
const QUERY_EXTENSION: u8 = 98;

/// A reply's type byte, and the X Generic Event's.
const REPLY: u8 = 1;
const GENERIC_EVENT: u8 = 35;

/// The token that hides XInput 2 alone, the extension it keeps, and
/// `XIQueryVersion`'s minor opcode.
const XINPUT2: &str = "XInput2";
const XINPUT: &str = "XInputExtension";
const XI_QUERY_VERSION: u8 = 47;

/// The core protocol's `Request` error: a request the server does not know.
const BAD_REQUEST: u8 = 1;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

fn pad4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// The sequence numbers of the hidden `QueryExtension`s still unanswered,
/// with the name each asked for.
type Asked = Arc<Mutex<Vec<(u16, String)>>>;

/// Under [`XINPUT2`]: the unanswered `QueryExtension`s of XInputExtension,
/// the major opcode its answer gave, and the unanswered `XIQueryVersion`s.
#[derive(Default)]
struct Xi1 {
    queries: Vec<u16>,
    major: Option<u8>,
    versions: Vec<u16>,
}

type SharedXi1 = Arc<Mutex<Xi1>>;

/// The client's side: counts requests and notes the hidden queries.
struct Requests {
    setup: bool,
    seq: u16,
    hide: Vec<String>,
    asked: Asked,
    xi1: Option<SharedXi1>,
}

impl Filter for Requests {
    fn len(&mut self, buf: &[u8]) -> Result<Option<usize>, String> {
        if !self.setup {
            let (Some(n), Some(d)) = (u16_at(buf, 6), u16_at(buf, 8)) else {
                return Ok(None);
            };
            if buf.first() != Some(&b'l') {
                return Err(
                    "a big-endian (or unknown) client, which this proxy does not speak".into(),
                );
            }
            return Ok(Some(12 + pad4(n.into()) + pad4(d.into())));
        }
        let Some(words) = u16_at(buf, 2) else {
            return Ok(None);
        };
        if words != 0 {
            return Ok(Some(usize::from(words) * 4));
        }
        match u32_at(buf, 4) {
            None => Ok(None),
            Some(0) => Err("a request of length 0".into()),
            Some(big) => usize::try_from(big)
                .ok()
                .and_then(|w| w.checked_mul(4))
                .map(Some)
                .ok_or_else(|| format!("a request of {big} words")),
        }
    }

    fn pass(&mut self, msg: &mut [u8]) -> bool {
        if !self.setup {
            self.setup = true;
            return true;
        }
        self.seq = self.seq.wrapping_add(1);
        if msg.first() == Some(&QUERY_EXTENSION) {
            let n = u16_at(msg, 4).map_or(0, usize::from);
            let name = msg
                .get(8..8 + n)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            if let Some(xi1) = self.xi1.as_ref().filter(|_| name == XINPUT) {
                xi1.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .queries
                    .push(self.seq);
            }
            if self.hide.contains(&name) {
                self.asked
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((self.seq, name));
            }
        } else if let Some(xi1) = &self.xi1 {
            let mut xi1 = xi1.lock().unwrap_or_else(PoisonError::into_inner);
            if xi1.major.is_some_and(|major| msg.first() == Some(&major))
                && msg.get(1) == Some(&XI_QUERY_VERSION)
            {
                xi1.versions.push(self.seq);
            }
        }
        true
    }
}

/// The server's side: answers each hidden query as absent, and under
/// [`XINPUT2`] each `XIQueryVersion` with a `Request` error.
struct Replies {
    setup: bool,
    asked: Asked,
    xi1: Option<SharedXi1>,
}

impl Filter for Replies {
    fn len(&mut self, buf: &[u8]) -> Result<Option<usize>, String> {
        if !self.setup {
            return Ok(u16_at(buf, 6).map(|w| 8 + usize::from(w) * 4));
        }
        if buf.len() < 32 {
            return Ok(None);
        }
        match buf.first() {
            Some(&REPLY | &GENERIC_EVENT) => {
                let words = u32_at(buf, 4).unwrap_or(0);
                usize::try_from(words)
                    .ok()
                    .and_then(|w| w.checked_mul(4))
                    .and_then(|b| b.checked_add(32))
                    .map(Some)
                    .ok_or_else(|| format!("a reply of {words} extra words"))
            }
            _ => Ok(Some(32)),
        }
    }

    fn pass(&mut self, msg: &mut [u8]) -> bool {
        if !self.setup {
            self.setup = true;
            return true;
        }
        if msg.first() != Some(&REPLY) {
            return true;
        }
        let Some(seq) = u16_at(msg, 2) else {
            return true;
        };
        if let Some(xi1) = &self.xi1 {
            let mut xi1 = xi1.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(i) = xi1.queries.iter().position(|s| *s == seq) {
                xi1.queries.remove(i);
                if msg.get(8) == Some(&1) {
                    xi1.major = msg.get(9).copied();
                }
            } else if let Some(i) = xi1.versions.iter().position(|s| *s == seq) {
                xi1.versions.remove(i);
                // The reply is 32 bytes (no extra words), as the error is.
                if let (Some(major), 32) = (xi1.major, msg.len()) {
                    msg.fill(0);
                    msg[1] = BAD_REQUEST;
                    msg[2..4].copy_from_slice(&seq.to_le_bytes());
                    msg[8] = XI_QUERY_VERSION;
                    msg[10] = major;
                    eprintln!("lane-bus: xhide: answered XIQueryVersion: Request error");
                }
            }
        }
        let mut asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = asked.iter().position(|(s, _)| *s == seq) {
            let (_, name) = asked.remove(i);
            if let Some(fields) = msg.get_mut(8..12) {
                fields.fill(0);
                eprintln!("lane-bus: xhide: answered QueryExtension {name}: absent");
            }
        }
        true
    }
}

/// The socket of X display `display` (`:93` or `93`).
fn socket(display: &str) -> Result<std::path::PathBuf, String> {
    let n = display.trim_start_matches(':');
    let n = n.split('.').next().unwrap_or(n);
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("not a local X display: {display}"));
    }
    Ok(Path::new("/tmp/.X11-unix").join(format!("X{n}")))
}

pub(crate) fn run(display: &str, server: &str, hide: &str) -> Result<(), String> {
    let hide: Vec<String> = if hide == "-" {
        Vec::new()
    } else {
        hide.split(',').map(str::to_string).collect()
    };
    eprintln!(
        "lane-bus: xhide: hiding {}",
        if hide.is_empty() {
            "nothing".to_string()
        } else {
            hide.join(", ")
        }
    );
    let xinput1 = hide.iter().any(|h| h == XINPUT2);
    relay::serve(&socket(display)?, &socket(server)?, || {
        let asked = Asked::default();
        let xi1 = xinput1.then(SharedXi1::default);
        (
            Requests {
                setup: false,
                seq: 0,
                hide: hide.clone(),
                asked: Arc::clone(&asked),
                xi1: xi1.clone(),
            },
            Replies {
                setup: false,
                asked,
                xi1,
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_request() -> Vec<u8> {
        let mut s = vec![b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        s[6] = 3; // a 3-byte authorization name, padded to 4
        s.extend_from_slice(b"abc\0");
        s
    }

    fn query(name: &str) -> Vec<u8> {
        let mut q = vec![QUERY_EXTENSION, 0, 0, 0];
        q.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        q.extend_from_slice(&[0, 0]);
        q.extend_from_slice(name.as_bytes());
        q.resize(pad4(q.len()), 0);
        let words = u16::try_from(q.len() / 4).unwrap();
        q[2..4].copy_from_slice(&words.to_le_bytes());
        q
    }

    fn reply(seq: u16) -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[0] = REPLY;
        r[2..4].copy_from_slice(&seq.to_le_bytes());
        r[8..12].copy_from_slice(&[1, 131, 80, 150]);
        r
    }

    /// Feed `stream` through `f` whole, as the relay cuts it.
    fn through(f: &mut dyn Filter, mut stream: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(n) = f.len(stream).unwrap() {
            let mut m = stream[..n].to_vec();
            if f.pass(&mut m) {
                out.extend_from_slice(&m);
            }
            stream = &stream[n..];
        }
        assert!(stream.is_empty(), "{} bytes left", stream.len());
        out
    }

    #[test]
    fn only_the_hidden_extension_is_answered_absent() {
        let asked = Asked::default();
        let mut req = Requests {
            setup: false,
            seq: 0,
            hide: vec!["XInputExtension".into()],
            asked: Arc::clone(&asked),
            xi1: None,
        };
        let mut stream = setup_request();
        stream.extend(query("RANDR"));
        stream.extend(query("XInputExtension"));
        // A big request (length 0, 32-bit length 3 words) between.
        stream.extend([127, 0, 0, 0, 3, 0, 0, 0, 9, 9, 9, 9]);
        stream.extend(query("XKEYBOARD"));
        assert_eq!(
            through(&mut req, &stream),
            stream,
            "requests pass unchanged"
        );
        assert_eq!(*asked.lock().unwrap(), [(2, "XInputExtension".to_string())]);

        let mut rep = Replies {
            setup: false,
            asked: Arc::clone(&asked),
            xi1: None,
        };
        let mut setup = vec![1, 0, 11, 0, 0, 0, 2, 0];
        setup.extend([7u8; 8]);
        let mut stream = setup.clone();
        for seq in 1..=4 {
            stream.extend(reply(seq));
        }
        // An XGE event with 2 extra words, and a core event.
        let mut xge = vec![GENERIC_EVENT; 1];
        xge.resize(32, 0);
        xge[4] = 2;
        xge.extend([5u8; 8]);
        stream.extend(&xge);
        stream.extend([2u8; 32]);
        let out = through(&mut rep, &stream);
        let mut want = setup;
        for seq in 1..=4 {
            let mut r = reply(seq);
            if seq == 2 {
                r[8..12].fill(0);
            }
            want.extend(r);
        }
        want.extend(&xge);
        want.extend([2u8; 32]);
        assert_eq!(out, want);
        assert!(asked.lock().unwrap().is_empty(), "the query was answered");
    }

    /// Under `XInput2`: XInputExtension is answered present, its opcode
    /// kept, and only its `XIQueryVersion` is answered with a `Request`
    /// error; a reply to another of its requests passes.
    #[test]
    fn xinput2_alone_is_hidden_as_an_xinput_1_server_lacks_it() {
        let asked = Asked::default();
        let xi1 = SharedXi1::default();
        let mut req = Requests {
            setup: false,
            seq: 0,
            hide: vec![XINPUT2.into()],
            asked: Arc::clone(&asked),
            xi1: Some(Arc::clone(&xi1)),
        };
        let mut rep = Replies {
            setup: false,
            asked: Arc::clone(&asked),
            xi1: Some(Arc::clone(&xi1)),
        };
        let mut setup = vec![1, 0, 11, 0, 0, 0, 2, 0];
        setup.extend([7u8; 8]);
        // The setup both ways, then QueryExtension (seq 1) and its answer
        // (opcode 131), then XIQueryVersion (seq 2), XIGetClientPointer
        // (seq 3, minor 45) and a core request (seq 4, opcode 47).
        through(&mut req, &setup_request());
        through(&mut rep, &setup);
        through(&mut req, &query(XINPUT));
        assert_eq!(
            through(&mut rep, &reply(1)),
            reply(1),
            "XInputExtension stays present"
        );
        let xi_query_version = [131, XI_QUERY_VERSION, 2, 0, 2, 0, 3, 0];
        let xi_get_client_pointer = [131, 45, 2, 0, 0, 0, 0, 0];
        let core = [XI_QUERY_VERSION, 0, 2, 0, 0, 0, 0, 0];
        for r in [&xi_query_version[..], &xi_get_client_pointer, &core] {
            assert_eq!(through(&mut req, r), r, "requests pass unchanged");
        }
        let mut error = vec![0u8; 32];
        error[1] = BAD_REQUEST;
        error[2] = 2;
        error[8] = XI_QUERY_VERSION;
        error[10] = 131;
        assert_eq!(through(&mut rep, &reply(2)), error);
        assert_eq!(through(&mut rep, &reply(3)), reply(3));
        assert_eq!(through(&mut rep, &reply(4)), reply(4));
        assert!(asked.lock().unwrap().is_empty(), "nothing answered absent");
        assert!(xi1.lock().unwrap().versions.is_empty());
    }

    #[test]
    fn a_big_endian_client_and_a_zero_length_request_are_refused() {
        let mut req = Requests {
            setup: false,
            seq: 0,
            hide: Vec::new(),
            asked: Asked::default(),
            xi1: None,
        };
        let mut b = setup_request();
        b[0] = b'B';
        assert!(req.len(&b).is_err());
        req.setup = true;
        assert!(req.len(&[1, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert_eq!(socket(":93").unwrap(), Path::new("/tmp/.X11-unix/X93"));
        assert!(socket("host:0").is_err());
    }
}
