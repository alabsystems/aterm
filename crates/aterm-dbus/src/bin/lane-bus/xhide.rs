// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-bus xhide`: X11 proxy that hides named extensions or `XInput2` from clients.

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
#[path = "xhide_tests.rs"]
mod tests;
