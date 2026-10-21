// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `xseat`'s input device: the XTEST extension's `FakeInput` (X.Org's "XTEST
//! Extension Protocol", version 2.2) on the X display `DISPLAY` names. weston's
//! x11 backend, nested in the lane's Xvfb, reads its seat's keyboard and
//! pointer from its own X window, which covers the X screen, so a key or a
//! motion faked here reaches the Wayland client weston gives the focus.
//!
//! Spoken directly, as [`crate::xdnd`] speaks the core protocol: not xdotool,
//! whose keysym lookup rebinds a spare keycode for a keysym the keymap lacks,
//! which a nested weston (it builds its keymap from the X server's rule names
//! once, as it starts) would read as another key. A keysym here is pressed
//! only on a key whose first level holds it, or refused. Every byte the server
//! sends is untrusted: a short or malformed reply is an error, never a panic.

use crate::xdnd::{Error, X, fault, request, u16_at, u32_at};

/// The core events `FakeInput` fakes, by code.
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;

/// XTEST's `FakeInput` minor opcode.
const FAKE_INPUT: u8 = 2;

/// The keysyms a keymap reply may hold per keycode; a larger count is a
/// malformed reply.
const MAX_PER_KEYCODE: usize = 64;

/// `QueryExtension` (98) of `name`.
fn query_extension(name: &str) -> Vec<u8> {
    let len = u16::try_from(name.len()).unwrap_or(u16::MAX);
    let mut body = len.to_le_bytes().to_vec();
    body.extend_from_slice(&[0, 0]);
    body.extend_from_slice(name.as_bytes());
    request(98, 0, &body)
}

/// `GetKeyboardMapping` (101) of `count` keycodes from `first`.
fn get_keyboard_mapping(first: u8, count: u8) -> Vec<u8> {
    request(101, 0, &[first, count, 0, 0])
}

/// `GetInputFocus` (43): a round trip, so every request sent before it has
/// been handled once its reply is read.
fn get_input_focus() -> Vec<u8> {
    request(43, 0, &[])
}

/// `QueryPointer` (38) of `window`.
fn query_pointer(window: u32) -> Vec<u8> {
    request(38, 0, &window.to_le_bytes())
}

/// `QueryKeymap` (44): which keys are down, a bit per keycode.
fn query_keymap() -> Vec<u8> {
    request(44, 0, &[])
}

/// The buttons a `QueryPointer` reply's key-and-button mask holds down
/// (`Button1Mask` is bit 8, through `Button5Mask`).
fn held_buttons(mask: u16) -> Vec<u8> {
    (1u8..=5).filter(|b| mask & (1 << (7 + b)) != 0).collect()
}

/// The keycodes a `QueryKeymap` reply's 32-byte bit vector holds down.
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

/// XTEST `FakeInput` of a core event: its code, its detail (a keycode, a
/// button, or for a motion 1 relative and 0 absolute), now (time 0), on the
/// pointer's screen (root None), at `(x, y)` for a motion.
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
    /// Read a `GetKeyboardMapping` reply of `count` keycodes from `min`.
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

    /// The lowest keycode whose first keysym (its first group's first level:
    /// what the key types with no modifier) is `keysym`.
    fn keycode(&self, keysym: u32) -> Option<u8> {
        self.syms
            .chunks(self.per)
            .position(|k| k.first() == Some(&keysym))
            .and_then(|i| u8::try_from(i).ok())
            .and_then(|i| self.min.checked_add(i))
    }
}

/// The X button of an evdev button code (BTN_LEFT, BTN_MIDDLE, BTN_RIGHT).
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

    /// Send one fake event, then a round trip: the server has handled it (and
    /// sent the window under the pointer its event) once this returns, and an
    /// error it raised is this call's.
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

    /// Let go of every button and key the X server holds down: what a session
    /// stopped mid-steps left pressed (the server keeps a faked press until
    /// its release comes, and weston, under a held button, keeps its
    /// pointer's focus on the surface the press began on, so the next window
    /// hears no `enter`: measured 2026-10-19, after a drag's session was
    /// stopped before its release). A RemoteDesktop session's devices go
    /// with the session; these stay with the server, so each session starts
    /// by letting go. The buttons and keys let go, by number.
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
mod tests {
    use super::*;

    /// Requests as XTEST 2.2 and the X11R7.7 core protocol encode them.
    #[test]
    fn requests_are_encoded_as_the_protocols_lay_them_out() {
        let q = query_extension("XTEST");
        assert_eq!(&q[..8], [98, 0, 4, 0, 5, 0, 0, 0]);
        assert_eq!(&q[8..13], b"XTEST");
        assert_eq!(q.len(), 16, "padded to a word");
        assert_eq!(get_keyboard_mapping(8, 248), [101, 0, 2, 0, 8, 248, 0, 0]);
        assert_eq!(get_input_focus(), [43, 0, 1, 0]);
        let f = fake_input(132, MOTION_NOTIFY, 1, -40, 30);
        assert_eq!(f.len(), 36, "FakeInput is nine words");
        assert_eq!(&f[..6], [132, FAKE_INPUT, 9, 0, MOTION_NOTIFY, 1]);
        assert_eq!(u32_at(&f, 8), Some(0), "time: now");
        assert_eq!(u32_at(&f, 12), Some(0), "root: the pointer's screen");
        assert_eq!(i16::from_le_bytes([f[24], f[25]]), -40, "rootX");
        assert_eq!(u16_at(&f, 26), Some(30), "rootY");
        assert_eq!(f[35], 0, "deviceid: the core devices");
        let k = fake_input(132, KEY_PRESS, 48, 0, 0);
        assert_eq!(&k[4..6], [KEY_PRESS, 48]);
    }

    #[test]
    fn a_keysym_is_found_on_a_keys_first_level_only() {
        // Two keysyms per keycode from 8: 8 a/A, 9 dead_acute/dead_diaeresis,
        // 10 apostrophe in the second level only, 11 a again.
        let mut reply = vec![1, 2, 0, 0, 8, 0, 0, 0];
        reply.resize(32, 0);
        for s in [0x61u32, 0x41, 0xfe51, 0xfe57, 0, 0x27, 0x61, 0x41] {
            reply.extend_from_slice(&s.to_le_bytes());
        }
        let map = Keymap::parse(&reply, 8, 4).expect("parses");
        assert_eq!(map.keycode(0x61), Some(8), "the lowest of two");
        assert_eq!(map.keycode(0xfe51), Some(9));
        assert_eq!(map.keycode(0x27), None, "a second level is not pressed");
        assert_eq!(map.keycode(0xffff), None);
        assert!(Keymap::parse(&reply, 8, 5).is_err(), "short of a fifth key");
        assert!(Keymap::parse(&reply[..1], 8, 1).is_err());
        let mut none = reply.clone();
        none[1] = 0;
        assert!(Keymap::parse(&none, 8, 4).is_err(), "no keysym per keycode");
    }

    #[test]
    fn what_the_server_holds_down_is_read_bit_by_bit() {
        // Button1Mask and Button3Mask, with Shift (bit 0) beside them.
        assert_eq!(held_buttons(0x0501), [1, 3]);
        assert_eq!(held_buttons(0x00ff), Vec::<u8>::new(), "modifiers only");
        let mut bits = [0u8; 32];
        bits[4] = 0b0100_0001; // keycodes 32 and 38
        bits[31] = 0x80; // keycode 255
        assert_eq!(held_keys(&bits), [32, 38, 255]);
        assert_eq!(held_keys(&[0; 32]), Vec::<u8>::new());
        assert_eq!(query_pointer(0x0505), [38, 0, 2, 0, 5, 5, 0, 0]);
        assert_eq!(query_keymap(), [44, 0, 1, 0]);
    }

    #[test]
    fn evdev_buttons_are_x_buttons() {
        assert_eq!(x_button(0x110), Some(1));
        assert_eq!(x_button(0x112), Some(2));
        assert_eq!(x_button(0x111), Some(3));
        assert_eq!(x_button(0x113), None);
    }
}
