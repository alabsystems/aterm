// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence border tones ([`tones`]) and in-grid pill text inks ([`inks`]).

use crate::ink::{BandInks, ForcedPalette, bg_is_light, ensure_contrast, forced_ink};

/// Presence tones for decide (`wait`), stuck (`stop`), and user-held (`yours`) states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceTones {
    /// A decide need: the wait tone (#B7811A light, #E0A83A dark).
    pub wait: [u8; 3],
    /// A stuck need: the stop tone (#C73F2C light, #E0533F dark).
    pub stop: [u8; 3],
    /// The person's own hold: the band's dim ink ([`BandInks::label`]).
    pub yours: [u8; 3],
}

/// Compute presence tones at 3:1 contrast against `c.bar_bg`, or High Contrast inks when `forced` is set.
#[must_use]
pub fn tones(theme_bg: [u8; 3], c: &BandInks, forced: Option<ForcedPalette>) -> PresenceTones {
    if let Some(hc) = forced {
        let ink = forced_ink(hc.window_text, hc.btn_face);
        return PresenceTones {
            wait: ink,
            stop: ink,
            yours: ink,
        };
    }
    let (wait, stop) = if bg_is_light(theme_bg) {
        (0x00B7_811A, 0x00C7_3F2C)
    } else {
        (0x00E0_A83A, 0x00E0_533F)
    };
    PresenceTones {
        wait: ensure_contrast(rgb(wait), c.bar_bg, 3.0),
        stop: ensure_contrast(rgb(stop), c.bar_bg, 3.0),
        yours: c.label,
    }
}

/// Floor `tones` to AA 4.5:1 text contrast against `bar_bg`.
#[must_use]
pub fn inks(tones: PresenceTones, bar_bg: [u8; 3]) -> PresenceTones {
    const AA: f64 = 4.5;
    PresenceTones {
        wait: ensure_contrast(tones.wait, bar_bg, AA),
        stop: ensure_contrast(tones.stop, bar_bg, AA),
        yours: ensure_contrast(tones.yours, bar_bg, AA),
    }
}

const fn rgb(c: u32) -> [u8; 3] {
    let [_, r, g, b] = c.to_be_bytes();
    [r, g, b]
}
