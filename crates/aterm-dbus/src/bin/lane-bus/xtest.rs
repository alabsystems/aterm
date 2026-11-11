// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! XTEST `FakeInput` client for synthesizing key, button, and pointer events on `$DISPLAY`.

use crate::xdnd::{Error, X, fault, request, u16_at, u32_at};

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;

const FAKE_INPUT: u8 = 2;

const MAX_PER_KEYCODE: usize = 64;

fn query_extension(name: &str) -> Vec<u8> {
    let len = u16::try_from(name.len()).unwrap_or(u16::MAX);
    let mut body = len.to_le_bytes().to_vec();
    body.extend_from_slice(&[0, 0]);
    body.extend_from_slice(name.as_bytes());
    request(98, 0, &body)
}

fn get_keyboard_mapping(first: u8, count: u8) -> Vec<u8> {
    request(101, 0, &[first, count, 0, 0])
}

fn get_input_focus() -> Vec<u8> {
    request(43, 0, &[])
}

fn query_pointer(window: u32) -> Vec<u8> {
    request(38, 0, &window.to_le_bytes())
}

fn query_keymap() -> Vec<u8> {
    request(44, 0, &[])
}

fn held_buttons(mask: u16) -> Vec<u8> {
    (1u8..=5).filter(|b| mask & (1 << (7 + b)) != 0).collect()
}

fn held_keys(bits: &[u8]) -> Vec<u8> {
    bits.iter()
        .take(32)
        .enumerate()
        .flat_map(|(i, byte)| {
            (0..8u8).filter_map(move |bit| {
                (byte & (1 << bit) != 0)
                    .then(|| u8::try_from(i * 8 + usize::from(bit)).ok())
                    .flatten()
            })
        })
        .collect()
}

fn fake_input(major: u8, kind: u8, detail: u8, x: i16, y: i16) -> Vec<u8> {
    let mut body = [0u8; 32];
    body[0] = kind;
    body[1] = detail;
    body[20..22].copy_from_slice(&x.to_le_bytes());
    body[22..24].copy_from_slice(&y.to_le_bytes());
    request(major, FAKE_INPUT, &body)
}

/// The X server's core keyboard mapping: each keycode's keysyms from `min`,
/// `per` of them.
#[derive(Debug, PartialEq, Eq)]
struct Keymap {
    min: u8,
    per: usize,
    syms: Vec<u32>,
}

impl Keymap {
    fn parse(reply: &[u8], min: u8, count: u8) -> Result<Keymap, String> {
        let per = usize::from(*reply.get(1).ok_or("a short GetKeyboardMapping reply")?);
        if per == 0 || per > MAX_PER_KEYCODE {
            return Err(format!("{per} keysyms per keycode"));
        }
        let n = per * usize::from(count);
        let syms = (0..n)
            .map(|i| u32_at(reply, 32 + 4 * i))
            .collect::<Option<Vec<u32>>>()
            .ok_or("a short GetKeyboardMapping reply")?;
        Ok(Keymap { min, per, syms })
    }

    fn keycode(&self, keysym: u32) -> Option<u8> {
        self.syms
            .chunks(self.per)
            .position(|k| k.first() == Some(&keysym))
            .and_then(|i| u8::try_from(i).ok())
            .and_then(|i| self.min.checked_add(i))
    }
}

fn x_button(code: i32) -> Option<u8> {
    match code {
        0x110 => Some(1),
        0x112 => Some(2),
        0x111 => Some(3),
        _ => None,
    }
}

/// A connection that fakes input with XTEST.
pub(crate) struct Xtest {
    x: X,
    major: u8,
    keymap: Keymap,
}

impl Xtest {
    /// Connect to `display`, find XTEST, and read the keyboard mapping.
    pub(crate) fn connect(display: &str) -> Result<Xtest, Error> {
        let mut x = X::connect(display)?;
        let r = x.call(&query_extension("XTEST"))?;
        let present = r.get(8).copied().unwrap_or(0);
        let major = r.get(9).copied().unwrap_or(0);
        if present == 0 || major == 0 {
            return Err(Error::NoServer(format!(
                "the X server on {display} has no XTEST"
            )));
        }
        let (min, max) = x.keycodes();
        let count = max
            .checked_sub(min)
            .and_then(|d| d.checked_add(1))
            .ok_or_else(|| fault(format!("keycodes {min}..{max}")))?;
        let reply = x.call(&get_keyboard_mapping(min, count))?;
        let keymap = Keymap::parse(&reply, min, count).map_err(fault)?;
        Ok(Xtest { x, major, keymap })
    }

    fn fake(&mut self, kind: u8, detail: u8, at: (i16, i16)) -> Result<(), Error> {
        self.x
            .send(&fake_input(self.major, kind, detail, at.0, at.1))?;
        self.x.call(&get_input_focus()).map(drop)
    }

    /// Press or release the key whose first level types `keysym`.
    pub(crate) fn key(&mut self, keysym: u32, pressed: bool) -> Result<(), Error> {
        let code = self.keymap.keycode(keysym).ok_or_else(|| {
            fault(format!(
                "keysym 0x{keysym:x} is on no key's first level of the X keymap"
            ))
        })?;
        let kind = if pressed { KEY_PRESS } else { KEY_RELEASE };
        self.fake(kind, code, (0, 0))
    }

    /// Press or release the evdev button `code`.
    pub(crate) fn button(&mut self, code: i32, pressed: bool) -> Result<(), Error> {
        let b =
            x_button(code).ok_or_else(|| fault(format!("button 0x{code:x} has no X button")))?;
        let kind = if pressed {
            BUTTON_PRESS
        } else {
            BUTTON_RELEASE
        };
        self.fake(kind, b, (0, 0))
    }

    /// Move the pointer by `(dx, dy)` screen pixels.
    pub(crate) fn motion(&mut self, dx: i32, dy: i32) -> Result<(), Error> {
        let d = |v: i32| {
            i16::try_from(v).map_err(|_| fault(format!("a motion of {v} px, past X's 16 bits")))
        };
        let at = (d(dx)?, d(dy)?);
        self.fake(MOTION_NOTIFY, 1, at)
    }

    /// Put the pointer in the screen's top-left corner.
    pub(crate) fn park(&mut self) -> Result<(), Error> {
        self.fake(MOTION_NOTIFY, 0, (0, 0))
    }

    /// Release any buttons and keys currently held down on the X server.
    pub(crate) fn let_go(&mut self) -> Result<(Vec<u8>, Vec<u8>), Error> {
        let root = self.x.root();
        let p = self.x.call(&query_pointer(root))?;
        let mask = u16_at(&p, 24).ok_or_else(|| fault("a short QueryPointer reply"))?;
        let buttons = held_buttons(mask);
        for &b in &buttons {
            self.fake(BUTTON_RELEASE, b, (0, 0))?;
        }
        let k = self.x.call(&query_keymap())?;
        let bits = k
            .get(8..40)
            .ok_or_else(|| fault("a short QueryKeymap reply"))?;
        let keys = held_keys(bits);
        for &code in &keys {
            self.fake(KEY_RELEASE, code, (0, 0))?;
        }
        Ok((buttons, keys))
    }
}

#[cfg(test)]
#[path = "xtest_tests.rs"]
mod tests;
