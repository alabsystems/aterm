// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A window's theme is its `_GTK_THEME_VARIANT`: `dark` and `light` for a chosen theme, and no hint
//! at all for `None`, the system's theme, so the window manager draws the frame in its own default,
//! the desktop's.

use aterm_x11::proto::{atom, xproto, NONE};
use aterm_x11::Connection;

use super::window::theme_variant;
use super::Window;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::{Theme, WindowAttributes};

/// A chosen theme asks for its variant; the system's theme asks for none.
#[test]
fn a_theme_is_its_variant_and_the_systems_is_none() {
    assert_eq!(theme_variant(Some(Theme::Dark)), Some("dark"));
    assert_eq!(theme_variant(Some(Theme::Light)), Some("light"));
    assert_eq!(theme_variant(None), None, "the desktop's theme is no variant, not dark");
}

/// `window`'s `_GTK_THEME_VARIANT` as the server holds it after a round trip of the window's own
/// connection: `None` where the window has none.
fn variant(watcher: &Connection, hint: u32, window: &Window) -> Option<String> {
    window.xconn.sync_with_server().expect("the window's requests reach the server");
    let reply = watcher
        .send(xproto::get_property(false, window.id().0 as u32, hint, atom::ANY, 0, 16))
        .unwrap()
        .reply()
        .expect("GetProperty _GTK_THEME_VARIANT");
    (reply.type_ != NONE).then(|| String::from_utf8_lossy(&reply.value).into_owned())
}

#[test]
fn the_variant_hint_follows_the_theme_and_the_systems_theme_removes_it() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (watcher, _) = Connection::connect(None).expect("DISPLAY is set: the connection must open");
    let [hint] = watcher.intern_atoms(&[b"_GTK_THEME_VARIANT"]).unwrap()[..] else {
        unreachable!()
    };

    let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
        panic!("an X11 loop");
    };
    let window = Window::new(target, WindowAttributes::default().with_visible(false))
        .expect("an X11 window");
    assert_eq!(variant(&watcher, hint, &window), None, "a new window asks for no variant");

    window.set_theme(Some(Theme::Light));
    assert_eq!(variant(&watcher, hint, &window).as_deref(), Some("light"));
    window.set_theme(Some(Theme::Dark));
    assert_eq!(variant(&watcher, hint, &window).as_deref(), Some("dark"));
    window.set_theme(None);
    assert_eq!(
        variant(&watcher, hint, &window),
        None,
        "the system's theme takes the hint away, the desktop's default drawing the frame"
    );
    window.set_theme(Some(Theme::Light));
    assert_eq!(variant(&watcher, hint, &window).as_deref(), Some("light"), "and a theme sets it");
}
