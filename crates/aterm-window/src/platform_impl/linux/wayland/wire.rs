// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Owned Wayland proxy wrapper and ordered event inbox for the Wayland backend.

use std::sync::{Arc, Mutex, PoisonError};

use aterm_wayland::output::OutputInfo;
use aterm_wayland::proxy::{EventArg, Listener, Proxy};
use aterm_wayland::seat::Input;
use aterm_wayland::selection::{DragEvent, SourceEvent};
use aterm_wayland::sink::Sink;

use super::{WaylandSelection, WindowId};

/// Owned Wayland proxy and optional listener destroyed together on drop.
pub(crate) struct Owned {
    proxy: Proxy,
    destroy: unsafe fn(Proxy),
    listener: Option<Listener>,
}

impl Owned {
    /// Own `proxy`, with no listener (an object with no events).
    ///
    /// # Safety
    /// `proxy` is a live object this backend made, owned nowhere else, and `destroy` is its
    /// destructor request (or [`Proxy::destroy`]).
    pub(crate) unsafe fn new(proxy: Proxy, destroy: unsafe fn(Proxy)) -> Owned {
        Owned { proxy, destroy, listener: None }
    }

    /// Owns `proxy` and records its decoded events in `inbox`.
    ///
    /// # Safety
    /// As [`Owned::new`], and `proxy` must have no listener yet. `decode` must not leave a
    /// descriptor argument unclosed (none of the objects this backend listens to sends one).
    pub(crate) unsafe fn listened(
        proxy: Proxy,
        destroy: unsafe fn(Proxy),
        inbox: &Inbox,
        decode: impl Fn(Proxy, u32, &[EventArg<'_>]) -> Option<Wire> + Send + 'static,
    ) -> Owned {
        let inbox = inbox.clone();
        // SAFETY: the caller's contract; the listener is freed after the object is destroyed
        // (Drop).
        let listener = unsafe {
            proxy.listen(move |object, opcode, args| {
                if let Some(wire) = decode(object, opcode, args) {
                    inbox.push(wire);
                }
            })
        };
        let listener = match listener {
            Ok(listener) => Some(listener),
            Err(_error) => {
                tracing::warn!("wayland: an object has no listener: {_error}");
                None
            },
        };
        Owned { proxy, destroy, listener }
    }

    /// Owns `proxy` and dispatches its events to `handler`.
    ///
    /// # Safety
    /// As [`Owned::listened`].
    pub(crate) unsafe fn handled(
        proxy: Proxy,
        destroy: unsafe fn(Proxy),
        handler: impl Fn(Proxy, u32, &[EventArg<'_>]) + Send + 'static,
    ) -> Owned {
        // SAFETY: caller guarantees `proxy` is valid; listener is freed after proxy destruction.
        let listener =
            unsafe { proxy.listen(move |object, opcode, args| handler(object, opcode, args)) };
        let listener = match listener {
            Ok(listener) => Some(listener),
            Err(_error) => {
                tracing::warn!("wayland: an object has no listener: {_error}");
                None
            },
        };
        Owned { proxy, destroy, listener }
    }

    /// The object.
    pub(crate) fn proxy(&self) -> Proxy {
        self.proxy
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: an object this value owns, destroyed once; its listener is freed after.
        unsafe { (self.destroy)(self.proxy) };
        drop(self.listener.take());
    }
}

impl std::fmt::Debug for Owned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Owned").field(&self.proxy).finish()
    }
}

/// Ordered event queue shared across Wayland object listeners.
#[derive(Clone, Default)]
pub(crate) struct Inbox {
    wire_book: Arc<Mutex<Vec<Wire>>>,
}

impl Inbox {
    fn push(&self, wire: Wire) {
        self.wire_book.lock().unwrap_or_else(PoisonError::into_inner).push(wire);
    }

    /// A sink for one of aterm-wayland's objects: each event it hears is recorded here as
    /// `wire` makes it, in the order the dispatch heard it among every other object's.
    fn sink<T: 'static>(&self, wire: impl Fn(T) -> Wire + Send + Sync + 'static) -> Sink<T> {
        let inbox = self.clone();
        Arc::new(move |event| inbox.push(wire(event)))
    }

    /// The sink aterm-wayland's `Outputs` records each output's state in, at its `done`.
    pub(crate) fn output_sink(&self) -> Sink<OutputInfo> {
        self.sink(Wire::OutputDone)
    }

    /// The sink aterm-wayland's `Seats` records every seat's input in.
    pub(crate) fn seat_sink(&self) -> Sink<Input> {
        self.sink(Wire::Seat)
    }

    /// The sink seat `seat`'s data devices record their drags in.
    pub(crate) fn drag_sink(&self, seat: u32) -> Sink<DragEvent> {
        self.sink(move |event| Wire::Drag { seat, event })
    }

    /// Returns a sink for clipboard selection source `source` events.
    pub(crate) fn source_sink(
        &self,
        selection: WaylandSelection,
        source: u64,
    ) -> Sink<SourceEvent> {
        self.sink(move |event| Wire::Source { selection, source, event })
    }

    /// Everything heard since the last take, in order.
    pub(crate) fn take(&self) -> Vec<Wire> {
        std::mem::take(&mut *self.wire_book.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// One event of an object the backend made.
#[derive(Debug)]
pub(crate) enum Wire {
    /// The window's `xdg_toplevel`, `xdg_surface` or toplevel decoration.
    Toplevel { window: WindowId, event: ToplevelEvent },
    /// A `wl_surface` of the backend's own: a window's, or a pointer's cursor surface.
    Surface { surface: WindowId, event: SurfaceEvent },
    /// `wp_fractional_scale_v1.preferred_scale`: the window's scale, in 120ths.
    PreferredScale { window: WindowId, scale: u32 },
    /// The seat's `zwp_text_input_v3`.
    TextInput { seat: u32, event: TextInputEvent },
    /// The window's attention request has its activation token.
    ActivationDone { window: WindowId, token: String },
    /// An output's `done`, with what it said as of it: aterm-wayland's `Outputs` records it here.
    OutputDone(OutputInfo),
    /// A seat's input: aterm-wayland's `Seats` records it here.
    Seat(Input),
    /// A drag over the seat's data device.
    Drag { seat: u32, event: DragEvent },
    /// A request to clipboard source `source` for `selection`.
    Source { selection: WaylandSelection, source: u64, event: SourceEvent },
}

#[derive(Debug)]
pub(crate) enum ToplevelEvent {
    /// `xdg_toplevel.configure`: the suggested size and the `xdg_toplevel.state` values.
    Configure {
        width: i32,
        height: i32,
        states: Vec<u32>,
    },
    Close,
    /// `xdg_toplevel.configure_bounds`.
    Bounds {
        width: i32,
        height: i32,
    },
    /// `xdg_toplevel.wm_capabilities`.
    Capabilities(Vec<u32>),
    /// `zxdg_toplevel_decoration_v1.configure`.
    DecorationMode(u32),
    /// `xdg_surface.configure`, already acked: the configure is whole.
    Configured,
}

#[derive(Debug)]
pub(crate) enum SurfaceEvent {
    /// `wl_surface.enter`: the output (`None` for one this client never bound).
    Enter(Option<Proxy>),
    Leave(Option<Proxy>),
    /// `wl_surface.preferred_buffer_scale` (version 6).
    PreferredBufferScale(i32),
}

#[derive(Debug)]
pub(crate) enum TextInputEvent {
    Enter(Option<Proxy>),
    Leave(Option<Proxy>),
    PreeditString { text: Option<String>, cursor_begin: i32, cursor_end: i32 },
    CommitString(Option<String>),
    Done,
}

/// A `wl_surface` event, decoded.
pub(crate) fn surface_event(opcode: u32, args: &[EventArg<'_>]) -> Option<SurfaceEvent> {
    use aterm_wayland::protocols::wayland::wl_surface::evt;
    Some(match (opcode, args) {
        (evt::ENTER, [EventArg::Object(output)]) => SurfaceEvent::Enter(*output),
        (evt::LEAVE, [EventArg::Object(output)]) => SurfaceEvent::Leave(*output),
        (evt::PREFERRED_BUFFER_SCALE, [EventArg::Int(factor)]) => {
            SurfaceEvent::PreferredBufferScale(*factor)
        },
        _ => return None,
    })
}

/// The native-endian `u32`s of a `wl_array`.
pub(crate) fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.as_chunks::<4>().0.iter().map(|w| u32::from_ne_bytes(*w)).collect()
}

/// A protocol string, as text.
pub(crate) fn text(s: Option<&std::ffi::CStr>) -> Option<String> {
    s.map(|s| s.to_string_lossy().into_owned())
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
