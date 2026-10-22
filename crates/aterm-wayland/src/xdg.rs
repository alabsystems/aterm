// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! xdg-shell windows: the `xdg_wm_base` (answering the compositor's pings),
//! a toplevel over a `wl_surface` (its `xdg_surface` and `xdg_toplevel`), the
//! configure sequence the compositor drives it with, and the decoration mode
//! negotiated through xdg-decoration where the compositor offers it. Where it
//! does not, the window decorates itself (aterm-gui's chrome band is the
//! header, LW.5).
//!
//! A configure is applied in two halves, as the protocol has it: the
//! toplevel's `configure` (size, states) and the surface's `configure`
//! (serial) that closes it; [`Toplevel::take_configure`] hands out the
//! whole, and the caller acks it once it has drawn for it.

use std::ffi::CStr;
use std::sync::{Arc, Mutex, PoisonError};

use crate::globals::Registry;
use crate::protocols::xdg_decoration_unstable_v1::{
    zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
};
use crate::protocols::xdg_shell::{xdg_surface, xdg_toplevel, xdg_wm_base};
use crate::proxy::{Error, EventArg, Listener, Proxy};

/// The highest `xdg_wm_base` version bound: smithay-client-toolkit 0.19.2's
/// (the winit fork's shell until LW.7), so a toplevel hears the configure
/// states it heard (version 7's constrained edges are not sent).
pub const WM_BASE_VERSION: u32 = 6;

/// The shell: `xdg_wm_base` (answering pings) and, when offered, the
/// decoration manager.
pub struct Shell {
    wm_base: Proxy,
    decorations: Option<Proxy>,
    _ping: Listener,
}

impl Shell {
    /// Bind `xdg_wm_base` (at most version [`WM_BASE_VERSION`]) and
    /// `zxdg_decoration_manager_v1` if the registry lists it.
    pub fn bind(registry: &Registry) -> Result<Shell, Error> {
        let global = registry.find(c"xdg_wm_base").ok_or(Error::NoObject)?;
        let wm_base = registry
            .bind(&global, &xdg_wm_base::INTERFACE, WM_BASE_VERSION)
            .ok_or(Error::NoObject)?;
        let pong = move |base: Proxy, opcode: u32, args: &[EventArg<'_>]| {
            if let (xdg_wm_base::evt::PING, [EventArg::Uint(serial)]) = (opcode, args) {
                // SAFETY: the live wm_base the event came from.
                unsafe { xdg_wm_base::pong(base, *serial) };
            }
        };
        // SAFETY: the wm_base just bound, without a listener; the listener
        // lives in the returned Shell and drops after the proxy.
        let ping = unsafe { wm_base.listen(pong) };
        let ping = match ping {
            Ok(l) => l,
            Err(e) => {
                // SAFETY: the wm_base bound above, not used again.
                unsafe { xdg_wm_base::destroy(wm_base) };
                return Err(e);
            }
        };
        let decorations = registry
            .find(c"zxdg_decoration_manager_v1")
            .and_then(|g| registry.bind(&g, &zxdg_decoration_manager_v1::INTERFACE, 1));
        Ok(Shell {
            wm_base,
            decorations,
            _ping: ping,
        })
    }

    /// Whether the compositor offers xdg-decoration.
    #[inline]
    pub const fn has_decorations(&self) -> bool {
        self.decorations.is_some()
    }

    /// The `xdg_wm_base`, for a caller that makes its toplevels' objects
    /// itself (the winit fork's windows, whose configure it hears in order
    /// with their scale); live while the shell is.
    #[inline]
    pub const fn wm_base(&self) -> Proxy {
        self.wm_base
    }

    /// The `zxdg_decoration_manager_v1`, when the compositor offers one; live
    /// while the shell is.
    #[inline]
    pub const fn decoration_manager(&self) -> Option<Proxy> {
        self.decorations
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        // SAFETY: objects this value bound, each destroyed once (every
        // toplevel made from them is gone: Toplevel borrows the Shell).
        unsafe {
            if let Some(d) = self.decorations {
                zxdg_decoration_manager_v1::destroy(d);
            }
            xdg_wm_base::destroy(self.wm_base);
        }
    }
}

/// Who draws the window's frame, as negotiated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decorations {
    /// The compositor offers no xdg-decoration: the client draws it.
    ClientUnoffered,
    /// Negotiated: the client draws it.
    Client,
    /// Negotiated: the compositor draws it.
    Server,
    /// Asked, not yet answered.
    Pending,
}

/// One complete configure: the toplevel's suggestion and the surface's
/// serial.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Configure {
    pub serial: u32,
    /// The suggested size in surface-local coordinates; 0 leaves it to the
    /// client.
    pub width: i32,
    pub height: i32,
    /// `xdg_toplevel.state` values (maximized 1, fullscreen 2, resizing 3,
    /// activated 4, the tiled edges 5-8, suspended 9).
    pub states: Vec<u32>,
}

#[derive(Default)]
struct State {
    pending: Configure,
    ready: Option<Configure>,
    close_requested: bool,
    bounds: Option<(i32, i32)>,
    capabilities: Vec<u32>,
    decorations: Option<Decorations>,
}

/// A toplevel window over `surface`.
pub struct Toplevel<'s> {
    shell: &'s Shell,
    xdg_surface: Proxy,
    toplevel: Proxy,
    decoration: Option<Proxy>,
    toplevel_book: Arc<Mutex<State>>,
    _listeners: Vec<Listener>,
}

/// Owns a constructor's objects and installed listeners until a Toplevel
/// takes them. Later setup failures destroy children before their parents,
/// while all installed listeners are still alive.
struct PendingToplevel {
    xdg_surface: Option<Proxy>,
    toplevel: Option<Proxy>,
    decoration: Option<Proxy>,
    listeners: Vec<Listener>,
}

impl Drop for PendingToplevel {
    fn drop(&mut self) {
        // SAFETY: objects made by this constructor and not transferred to a
        // Toplevel; taken once, children first, before listeners are freed.
        unsafe {
            if let Some(d) = self.decoration.take() {
                zxdg_toplevel_decoration_v1::destroy(d);
            }
            if let Some(t) = self.toplevel.take() {
                xdg_toplevel::destroy(t);
            }
            if let Some(s) = self.xdg_surface.take() {
                xdg_surface::destroy(s);
            }
        }
    }
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_ne_bytes(*w))
        .collect()
}

/// Which of a toplevel's objects a listener hears.
#[derive(Clone, Copy)]
enum Part {
    Surface,
    Toplevel,
    Decoration,
}

/// One event of a toplevel's objects, decoded.
enum Heard {
    /// `xdg_surface.configure`: the configure is whole.
    Serial(u32),
    /// `xdg_toplevel.configure`.
    Suggest {
        width: i32,
        height: i32,
        states: Vec<u32>,
    },
    Close,
    Bounds(i32, i32),
    Capabilities(Vec<u32>),
    Decorations(Decorations),
}

fn heard(part: Part, opcode: u32, args: &[EventArg<'_>]) -> Option<Heard> {
    Some(match (part, opcode, args) {
        (Part::Surface, xdg_surface::evt::CONFIGURE, [EventArg::Uint(serial)]) => {
            Heard::Serial(*serial)
        }
        (
            Part::Toplevel,
            xdg_toplevel::evt::CONFIGURE,
            [EventArg::Int(w), EventArg::Int(h), EventArg::Array(states)],
        ) => Heard::Suggest {
            width: *w,
            height: *h,
            states: words(states),
        },
        (Part::Toplevel, xdg_toplevel::evt::CLOSE, []) => Heard::Close,
        (
            Part::Toplevel,
            xdg_toplevel::evt::CONFIGURE_BOUNDS,
            [EventArg::Int(w), EventArg::Int(h)],
        ) => Heard::Bounds(*w, *h),
        (Part::Toplevel, xdg_toplevel::evt::WM_CAPABILITIES, [EventArg::Array(caps)]) => {
            Heard::Capabilities(words(caps))
        }
        (Part::Decoration, zxdg_toplevel_decoration_v1::evt::CONFIGURE, [EventArg::Uint(m)]) => {
            Heard::Decorations(if *m == zxdg_toplevel_decoration_v1::mode::SERVER_SIDE {
                Decorations::Server
            } else {
                Decorations::Client
            })
        }
        _ => return None,
    })
}

impl State {
    fn hear(&mut self, heard: Heard) {
        match heard {
            Heard::Serial(serial) => {
                let mut whole = std::mem::take(&mut self.pending);
                whole.serial = serial;
                self.ready = Some(whole);
            }
            Heard::Suggest {
                width,
                height,
                states,
            } => {
                self.pending.width = width;
                self.pending.height = height;
                self.pending.states = states;
            }
            Heard::Close => self.close_requested = true,
            Heard::Bounds(w, h) => self.bounds = Some((w, h)),
            Heard::Capabilities(caps) => self.capabilities = caps,
            Heard::Decorations(d) => self.decorations = Some(d),
        }
    }
}

/// Listen to one of a toplevel's objects: each event, decoded with the book
/// unlocked, is applied to it. The book is a leaf of the lock graph: only its
/// own method runs while it is held.
///
/// # Safety
/// `proxy` is a live object of `part`'s kind this module made, without a
/// listener, destroyed before the listener is freed.
unsafe fn listen_part(
    proxy: Proxy,
    toplevel_book: &Arc<Mutex<State>>,
    part: Part,
) -> Result<Listener, Error> {
    let toplevel_book = Arc::clone(toplevel_book);
    // SAFETY: the caller's contract.
    unsafe {
        proxy.listen(move |_, opcode, args| {
            if let Some(h) = heard(part, opcode, args) {
                toplevel_book
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .hear(h);
            }
        })
    }
}

impl<'s> Toplevel<'s> {
    /// Give `surface` the toplevel role, titled, and ask for server-side
    /// decorations where the compositor negotiates them. The first configure
    /// follows the first commit.
    ///
    /// # Safety
    /// `surface` is a live `wl_surface` with no role, on the same display
    /// as the shell, and outlives the toplevel.
    pub unsafe fn new(
        shell: &'s Shell,
        surface: Proxy,
        title: &CStr,
        app_id: &CStr,
    ) -> Result<Toplevel<'s>, Error> {
        let toplevel_book = Arc::new(Mutex::new(State::default()));
        // SAFETY: the shell's live wm_base and the caller's role-less surface.
        let xdg = unsafe { xdg_wm_base::get_xdg_surface(shell.wm_base, surface) }
            .ok_or(Error::NoObject)?;
        let mut pending = PendingToplevel {
            xdg_surface: Some(xdg),
            toplevel: None,
            decoration: None,
            listeners: Vec::new(),
        };
        // SAFETY: an xdg_surface made here; its pending owner destroys it
        // before any installed listeners are freed on a later error.
        pending
            .listeners
            .push(unsafe { listen_part(xdg, &toplevel_book, Part::Surface) }?);
        // SAFETY: the live xdg_surface.
        let toplevel = unsafe { xdg_surface::get_toplevel(xdg) }.ok_or(Error::NoObject)?;
        pending.toplevel = Some(toplevel);
        // SAFETY: a toplevel made here, held by the pending owner.
        pending
            .listeners
            .push(unsafe { listen_part(toplevel, &toplevel_book, Part::Toplevel) }?);
        // SAFETY: the live toplevel; NUL-terminated strings.
        unsafe {
            xdg_toplevel::set_title(toplevel, title);
            xdg_toplevel::set_app_id(toplevel, app_id);
        }
        let decoration = match shell.decorations {
            Some(manager) => {
                // SAFETY: the shell's live manager and the toplevel above.
                let d = unsafe {
                    zxdg_decoration_manager_v1::get_toplevel_decoration(manager, toplevel)
                }
                .ok_or(Error::NoObject)?;
                pending.decoration = Some(d);
                // SAFETY: a decoration object made here, held by the pending owner.
                pending
                    .listeners
                    .push(unsafe { listen_part(d, &toplevel_book, Part::Decoration) }?);
                // SAFETY: the live decoration object.
                unsafe {
                    zxdg_toplevel_decoration_v1::set_mode(
                        d,
                        zxdg_toplevel_decoration_v1::mode::SERVER_SIDE,
                    );
                }
                Some(d)
            }
            None => None,
        };
        let ready = Toplevel {
            shell,
            xdg_surface: xdg,
            toplevel,
            decoration,
            toplevel_book,
            _listeners: std::mem::take(&mut pending.listeners),
        };
        // The complete owner exists before the pending owner is disarmed.
        pending.decoration = None;
        pending.toplevel = None;
        pending.xdg_surface = None;
        Ok(ready)
    }

    /// The last complete configure not yet taken.
    pub fn take_configure(&self) -> Option<Configure> {
        self.toplevel_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ready
            .take()
    }

    /// Acknowledge a configure (after drawing for it, before committing).
    pub fn ack(&self, serial: u32) {
        // SAFETY: the live xdg_surface this value made.
        unsafe { xdg_surface::ack_configure(self.xdg_surface, serial) }
    }

    /// Whether the compositor asked the window to close.
    pub fn close_requested(&self) -> bool {
        self.toplevel_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .close_requested
    }

    /// The compositor's bounds for the window, if it sent any.
    pub fn bounds(&self) -> Option<(i32, i32)> {
        self.toplevel_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bounds
    }

    /// The window-manager capabilities the compositor announced (version 5).
    pub fn capabilities(&self) -> Vec<u32> {
        self.toplevel_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .capabilities
            .clone()
    }

    /// Who draws the frame.
    pub fn decorations(&self) -> Decorations {
        if !self.shell.has_decorations() {
            return Decorations::ClientUnoffered;
        }
        self.toplevel_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .decorations
            .unwrap_or(Decorations::Pending)
    }

    /// The `xdg_toplevel`, for the requests this helper does not wrap.
    #[inline]
    pub const fn toplevel(&self) -> Proxy {
        self.toplevel
    }
}

impl Drop for Toplevel<'_> {
    fn drop(&mut self) {
        // SAFETY: objects this value made, destroyed once, children first;
        // the listeners drop after, with the fields.
        unsafe {
            if let Some(d) = self.decoration {
                zxdg_toplevel_decoration_v1::destroy(d);
            }
            xdg_toplevel::destroy(self.toplevel);
            xdg_surface::destroy(self.xdg_surface);
        }
    }
}
