// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/window_state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::WindowFlags;
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_MAXIMIZE, SW_SHOW, SW_SHOWNOACTIVATE};

#[test]
fn a_hidden_window_is_revealed_maximized_in_one_step() {
    let hidden = WindowFlags::MARKER_ACTIVATE | WindowFlags::RESIZABLE;
    let hidden_max = hidden | WindowFlags::MAXIMIZED;
    let shown = hidden | WindowFlags::VISIBLE;
    assert!(WindowFlags::maximize_waits_for_reveal(hidden_max));
    assert!(!WindowFlags::maximize_waits_for_reveal(shown | WindowFlags::MAXIMIZED));
    assert!(!WindowFlags::maximize_waits_for_reveal(hidden));
    assert_eq!(
        WindowFlags::show_command(hidden_max, hidden_max | WindowFlags::VISIBLE),
        SW_MAXIMIZE
    );
    assert_eq!(WindowFlags::show_command(hidden, shown), SW_SHOW);
    assert_eq!(WindowFlags::show_command(shown, shown | WindowFlags::MAXIMIZED), SW_SHOW);
    let passive = WindowFlags::MAXIMIZED;
    assert_eq!(
        WindowFlags::show_command(passive, passive | WindowFlags::VISIBLE),
        SW_SHOWNOACTIVATE
    );
}
