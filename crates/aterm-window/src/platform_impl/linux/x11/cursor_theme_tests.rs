// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! The pointer cursor follows the resource database's theme and size as they change. GNOME's
//! settings daemon rewrites `RESOURCE_MANAGER`'s `Xcursor.theme` and `Xcursor.size` live (Large
//! Cursor, a theme picked in Settings), then its XSETTINGS, whose change the event loop hears.
//! libXcursor read them once, at its first cursor, and the backend kept each shape it loaded for
//! the process's life, so neither change showed in aterm until a restart (the desk audit, GNOME
//! 46 on Xorg, 2026-10-22).
//!
//! A second connection stands in for the daemon: it rewrites `RESOURCE_MANAGER` on the display's
//! root with a size and a theme no one uses, and the change's PropertyNotify is handed to the
//! event processor's handler, as `focus_tests` hands it focus events. Each cursor load is
//! counted with the size and theme libXcursor holds at it (`util::CURSOR_LOADS`): the window's
//! cursor is loaded again at the new ones. The property is then put back as it was, and the
//! change handed on again puts libXcursor back to its own size and theme.
//!
//! The negative controls: without the cache's clearing in `follow_cursor_settings`, the window's
//! cursor is not loaded again; without `XcursorSetDefaultSize` it loads at the old size, and
//! without `XcursorSetTheme` in the old theme; without `reload_cursor` in the event loop's
//! settings change, it is not loaded again.
//!
//! It runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

use std::ffi::CString;

use aterm_x11::proto::xproto::{self, PropMode};
use aterm_x11::proto::{atom, event};
use aterm_x11::Connection;

use super::ffi::{self, XPropertyEvent};
use super::util::CURSOR_LOADS;
use super::Window;
use crate::cursor::Cursor;
use crate::event::Event;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::{CursorIcon, WindowAttributes};

/// A theme no one has. The window shows no cursor while it is the theme.
const THEME: &str = "aterm-test-no-such-cursor-theme";

/// The display's `RESOURCE_MANAGER`, as `watcher` reads it on `root`; `None` when it has none.
fn resources(watcher: &Connection, root: u32) -> Option<Vec<u8>> {
    let request =
        xproto::get_property(false, root, atom::RESOURCE_MANAGER, atom::STRING, 0, 100_000_000);
    let reply = watcher.send(request).unwrap().reply().expect("GetProperty RESOURCE_MANAGER");
    (reply.format == 8).then_some(reply.value)
}

/// Make the display's `RESOURCE_MANAGER` `value` (`None`: delete it); once this returns, the
/// server has it.
fn set_resources(watcher: &Connection, root: u32, value: Option<&[u8]>) {
    let request = match value {
        Some(value) => xproto::change_property(
            PropMode::Replace,
            root,
            atom::RESOURCE_MANAGER,
            atom::STRING,
            8,
            value,
        ),
        None => xproto::delete_property(root, atom::RESOURCE_MANAGER),
    };
    watcher.send_void(request).unwrap().check().expect("RESOURCE_MANAGER is written");
}

/// Puts `RESOURCE_MANAGER` back as it was, however the test ends.
struct PutBack<'a> {
    watcher: &'a Connection,
    root: u32,
    was: Option<Vec<u8>>,
}

impl Drop for PutBack<'_> {
    fn drop(&mut self) {
        set_resources(self.watcher, self.root, self.was.as_deref());
    }
}

/// The cursor loads since the last look.
fn loads() -> Vec<(CursorIcon, i32, Option<CString>)> {
    CURSOR_LOADS.with(|loads| std::mem::take(&mut *loads.borrow_mut()))
}

#[test]
fn a_changed_cursor_size_and_theme_load_the_windows_cursor_anew() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (watcher, screen) =
        Connection::connect(None).expect("DISPLAY is set: the connection must open");
    let root = watcher.setup().roots[screen].root;

    let mut event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let (window, own) = {
        let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
            panic!("an X11 loop");
        };
        let window = Window::new(target, WindowAttributes::default().with_visible(false))
            .expect("an X11 window");
        (window, target.x_connection().xcursor_settings())
    };
    window.set_cursor(Cursor::Icon(CursorIcon::Text));
    loads();

    let was = resources(&watcher, root);
    let put_back = PutBack { watcher: &watcher, root, was: was.clone() };
    // The settings daemon's rewrite: what was there, less any cursor setting, and the new ones.
    let size = if own.1 == 48 { 64 } else { 48 };
    let mut changed: Vec<u8> = String::from_utf8_lossy(was.as_deref().unwrap_or_default())
        .lines()
        .filter(|line| !line.starts_with("Xcursor."))
        .flat_map(|line| [line, "\n"])
        .collect::<String>()
        .into_bytes();
    changed.extend(format!("Xcursor.theme:\t{THEME}\nXcursor.size:\t{size}\n").bytes());
    set_resources(&watcher, root, Some(&changed));

    // SAFETY: a plain C struct; every pointer in it stays null and the handler reads none.
    let mut notify: XPropertyEvent = unsafe { std::mem::zeroed() };
    notify.type_ = ffi::event_type::PROPERTY_NOTIFY;
    notify.window = root as ffi::Window;
    notify.atom = atom::RESOURCE_MANAGER as ffi::Atom;
    notify.state = event::property::NEW_VALUE.into();
    let EventLoop::X(x11) = &mut event_loop else { panic!("an X11 loop") };
    let processor = &mut x11.event_processor;
    processor.property_notify(&notify, |_, _: Event<()>| {});
    let theme = CString::new(THEME).unwrap();
    assert_eq!(
        loads(),
        [(CursorIcon::Text, size, Some(theme))],
        "the window's cursor is loaded again, at the new size and theme"
    );

    // The settings put back: libXcursor's own again.
    drop(put_back);
    processor.property_notify(&notify, |_, _: Event<()>| {});
    assert_eq!(
        loads(),
        [(CursorIcon::Text, own.1, own.0)],
        "the cursor is loaded again, at libXcursor's own size and theme"
    );
}
