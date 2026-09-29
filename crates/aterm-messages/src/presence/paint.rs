// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The row's COLOURS and CELLS: the presence tones (the rim's hues and the
//! story dot, [`tones`]), the same hues as text inks ([`inks`]), and the
//! painted row ([`paint_row`]) — which character in which cell, in which ink,
//! on the chrome band's material ([`BandInks`]). The host maps the cells onto
//! its own cell type.

use super::{SlotKind, Tone, Words};
use crate::MARGIN;
use crate::ink::{BandInks, ForcedPalette, InkedCell, bg_is_light, ensure_contrast, forced_ink};

/// The PRESENCE tones: the rim's three hues and the story dot. The owner's
/// picks on the approved mocks: teal for DRIVE (a peer's hand is on the
/// keyboard), amber for WAIT (a human should look), red for STOP (held, or at
/// a limit), violet for a STORY (something happened while you were away).
/// Light and dark are the mocks' own pairs; each is contrast-floored against
/// the band, at the floor its surface owes: [`tones`] at the 3:1 non-text
/// floor a rim and a chip's dot share, [`inks`] at WCAG AA 4.5:1 for the
/// WORDS the row paints in them. One hue family, two floors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceTones {
    /// A peer's hand.
    pub drive: [u8; 3],
    /// A human should look.
    pub wait: [u8; 3],
    /// Held, or at a limit.
    pub stop: [u8; 3],
    /// Something happened while you were away.
    pub story: [u8; 3],
}

/// The presence tones on the band `c` of a theme whose background is
/// `theme_bg` — or, under an OS-forced chrome palette (`forced`), the High
/// Contrast vocabulary's own words for them. High Contrast discards hue as a
/// channel, so DRIVE is `COLOR_HIGHLIGHT` (a peer has this window: the
/// "selected" meaning is the honest one) and WAIT / STOP / STORY collapse
/// onto the one text ink — the row says which in words, and a hold's rim is
/// still twice as thick with its wash, so the states stay distinguishable
/// without a hue.
#[must_use]
pub fn tones(theme_bg: [u8; 3], c: &BandInks, forced: Option<ForcedPalette>) -> PresenceTones {
    if let Some(hc) = forced {
        let ink = forced_ink(hc.window_text, hc.btn_face);
        return PresenceTones {
            // `COLOR_HIGHLIGHT` is a FILL in the HC vocabulary (the selected
            // tab's), so as an ink or a rim it is floored like every other.
            drive: forced_ink(hc.highlight, hc.btn_face),
            wait: ink,
            stop: ink,
            story: ink,
        };
    }
    let (drive, wait, stop, story) = if bg_is_light(theme_bg) {
        (0x001E_9A8A, 0x00B7_811A, 0x00C7_3F2C, 0x006A_5FD0)
    } else {
        (0x002F_B7A6, 0x00E0_A83A, 0x00E0_533F, 0x008B_7FE6)
    };
    PresenceTones {
        drive: ensure_contrast(rgb(drive), c.bar_bg, 3.0),
        wait: ensure_contrast(rgb(wait), c.bar_bg, 3.0),
        stop: ensure_contrast(rgb(stop), c.bar_bg, 3.0),
        story: ensure_contrast(rgb(story), c.bar_bg, 3.0),
    }
}

/// The presence hues as TEXT inks on a band whose ground is `bar_bg`:
/// [`tones`]' hues held to the AA 4.5:1 text floor. What [`paint_row`] paints
/// the hand and phase slots in; the rim keeps the 3:1 tones. Under a forced
/// HC palette the three text inks are already `WINDOWTEXT` on `BTNFACE`;
/// `COLOR_HIGHLIGHT` — a FILL the OS never meant as ink — is the one that can
/// need the lift.
#[must_use]
pub fn inks(tones: PresenceTones, bar_bg: [u8; 3]) -> PresenceTones {
    const AA: f64 = 4.5;
    PresenceTones {
        drive: ensure_contrast(tones.drive, bar_bg, AA),
        wait: ensure_contrast(tones.wait, bar_bg, AA),
        stop: ensure_contrast(tones.stop, bar_bg, AA),
        story: ensure_contrast(tones.story, bar_bg, AA),
    }
}

/// A packed `0x00RRGGBB` colour as sRGB bytes.
const fn rgb(c: u32) -> [u8; 3] {
    let [_, r, g, b] = c.to_be_bytes();
    [r, g, b]
}

/// The cells of a `cols`-wide row that carry TEXT: the row keeps one margin
/// cell each side, and the `chrome` line and the a11y detail fit the words
/// at this width — the same line the human sees, never a slot past the margin.
#[must_use]
pub const fn text_cols(cols: usize) -> usize {
    cols.saturating_sub(2 * MARGIN)
}

/// Paint the row ([`Words`]) at `cols`: the six slots laid out by
/// [`Words::pieces`] (which sheds from the right in the design's order), on
/// the band's material `c`, each slot in its own ink — the role in `value`,
/// a `since`/fabric figure in `label`, the hand in the DRIVE hue while a peer
/// types (the rim's own teal family, so the two surfaces read as one fact —
/// at the TEXT floor, `p` from [`inks`], since these are words), `⊘ hold` in
/// STOP, the phase in WARN while the row's tone is `Warn` and in the drive
/// hue for the two seconds after a settled turn. The trust glyph and `⚠` are
/// text-presentation on purpose (a colour emoji would be two cells wide in
/// one). Exactly `cols` cells; the row carries no closing seam of its own.
#[must_use]
pub fn paint_row(words: &Words, cols: usize, c: &BandInks, p: &PresenceTones) -> Vec<InkedCell> {
    let mut row = vec![
        InkedCell {
            ch: ' ',
            fg: c.label,
            bg: c.bar_bg,
            bold: false,
            text_presentation: false,
        };
        cols
    ];
    let pieces = words.pieces(text_cols(cols));
    let mut col = MARGIN;
    for (i, (kind, text)) in pieces.iter().enumerate() {
        if i > 0 {
            col += if *kind == SlotKind::Since { 1 } else { 2 };
        }
        let (ink, bold) = match kind {
            SlotKind::Role => (c.value, true),
            SlotKind::Phase => match words.tone {
                Tone::Warn => (c.warn, false),
                Tone::Success => (p.drive, false),
                Tone::Info => (c.value, false),
            },
            SlotKind::Since => (c.label, false),
            SlotKind::Hand => {
                if text.starts_with('\u{2298}') {
                    (p.stop, true)
                } else if text.starts_with('\u{25c2}') || text.starts_with('\u{25b8}') {
                    (p.drive, false)
                } else {
                    (c.label, false)
                }
            }
            SlotKind::Mail => (c.value, false),
            SlotKind::Ctx => {
                if text.ends_with('\u{26a0}') {
                    (c.warn, false)
                } else {
                    (c.value, false)
                }
            }
            SlotKind::Fabric => {
                if text.starts_with('\u{2715}') {
                    (p.stop, false)
                } else if text.starts_with('~') {
                    (c.warn, false)
                } else {
                    (c.label, false)
                }
            }
        };
        for (k, ch) in text.chars().enumerate() {
            let Some(cell) = row.get_mut(col + k) else {
                break;
            };
            *cell = InkedCell {
                ch,
                fg: ink,
                bg: c.bar_bg,
                bold,
                // The glyphs that have an emoji form are pinned to one cell —
                // the fleet lock included: U+1F512 is emoji-presentation by
                // default, and a renderer would take its colour face two
                // cells wide otherwise.
                text_presentation: matches!(
                    ch,
                    '\u{2713}' | '\u{2717}' | '\u{26a0}' | '\u{2709}' | '\u{2715}' | '\u{1f512}'
                ),
            };
        }
        col += text.chars().count();
    }
    row
}
