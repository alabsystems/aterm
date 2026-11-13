// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::ForcedChrome;

/// The four stock Windows High Contrast schemes as `GetSysColor` reports them.
pub(crate) const STOCK: [(&str, ForcedChrome); 4] = [
    (
        "Aquatic",
        ForcedChrome {
            window: [0x00, 0x00, 0x00],
            window_text: [0xFF, 0xFF, 0xFF],
            highlight: [0x37, 0x00, 0x6E],
            highlight_text: [0xFF, 0xFF, 0xFF],
            btn_face: [0x00, 0x00, 0x00],
        },
    ),
    (
        "Desert",
        ForcedChrome {
            window: [0xFF, 0xFF, 0xFF],
            window_text: [0x00, 0x00, 0x00],
            highlight: [0x37, 0x00, 0x6E],
            highlight_text: [0xFF, 0xFF, 0xFF],
            btn_face: [0xFF, 0xFF, 0xFF],
        },
    ),
    (
        "Dusk",
        ForcedChrome {
            window: [0x2D, 0x32, 0x36],
            window_text: [0xFF, 0xFF, 0xFF],
            highlight: [0x1A, 0xEB, 0xFF],
            highlight_text: [0x00, 0x00, 0x00],
            btn_face: [0x2D, 0x32, 0x36],
        },
    ),
    (
        "Night sky",
        ForcedChrome {
            window: [0x00, 0x00, 0x00],
            window_text: [0xFF, 0xFF, 0xFF],
            highlight: [0x1A, 0xEB, 0xFF],
            highlight_text: [0x00, 0x00, 0x00],
            btn_face: [0x00, 0x00, 0x00],
        },
    ),
];

/// Install `palette` for `body` on this test thread, restoring the previous value.
pub(crate) fn with_forced<R>(palette: ForcedChrome, body: impl FnOnce() -> R) -> R {
    let previous = super::forced_chrome();
    super::install_forced_chrome(Some(palette));
    let out = body();
    super::install_forced_chrome(previous);
    out
}
