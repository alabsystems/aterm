// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The border's colours: the presence tones ([`tones`]) and the same hues as
//! text inks ([`inks`]) for the in-grid pill's words. Two alarm hues and no
//! more: the stop tone for a stuck need, the wait tone for a decide need;
//! the person's own hold wears the band's dim ink, no alarm hue at all.

use crate::ink::{BandInks, ForcedPalette, bg_is_light, ensure_contrast, forced_ink};

/// The presence tones: amber for a decide need, red for a stuck one, and
/// the dim ink of the person's own hold. Each alarm hue has a light and a
/// dark pair, contrast-floored against the band: [`tones`] at the 3:1
/// non-text floor a border and a chip's mark share, [`inks`] at AA 4.5:1 for
/// words painted in them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceTones {
    /// A decide need: the wait tone (#B7811A light, #E0A83A dark).
    pub wait: [u8; 3],
    /// A stuck need: the stop tone (#C73F2C light, #E0533F dark).
    pub stop: [u8; 3],
    /// The person's own hold: the band's dim ink ([`BandInks::label`]).
    pub yours: [u8; 3],
}

/// The presence tones on the band `c` of a theme whose background is
/// `theme_bg`, or under an OS-forced chrome palette (`forced`) the High
/// Contrast colours: hue is no channel there, so every tone is the text ink
/// and the pill's glyph and words say which.
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

/// The presence hues as text inks on a band whose ground is `bar_bg`:
/// [`tones`]' hues held to the AA 4.5:1 text floor, for the words the
/// in-grid pill paints; the border keeps the 3:1 tones.
#[must_use]
pub fn inks(tones: PresenceTones, bar_bg: [u8; 3]) -> PresenceTones {
    const AA: f64 = 4.5;
    PresenceTones {
        wait: ensure_contrast(tones.wait, bar_bg, AA),
        stop: ensure_contrast(tones.stop, bar_bg, AA),
        yours: ensure_contrast(tones.yours, bar_bg, AA),
    }
}

/// A packed `0x00RRGGBB` colour as sRGB bytes.
const fn rgb(c: u32) -> [u8; 3] {
    let [_, r, g, b] = c.to_be_bytes();
    [r, g, b]
}
