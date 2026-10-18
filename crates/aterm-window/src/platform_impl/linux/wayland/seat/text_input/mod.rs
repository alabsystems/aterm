// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/seat/text_input/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE. (text-input-v3 is
//! first-party aterm-wayland's table, and a seat's text input is the backend's own [`TextInput`],
//! where upstream had wayland-protocols' `ZwpTextInputV3` dispatched by wayland-client:
//! docs/AUDIT-third-party-2026-10-15.md, LW.7.)

use aterm_wayland::protocols::text_input_unstable_v3::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use aterm_wayland::proxy::{EventArg, Proxy};

use crate::event::{Ime, WindowEvent};
use crate::platform_impl::wayland;
use std::sync::Arc;

use crate::platform_impl::wayland::state::{Connection, WinitState};
use crate::platform_impl::wayland::wire::{self, Inbox, Owned, TextInputEvent, Wire};
use crate::platform_impl::wayland::WindowId;
use crate::window::ImePurpose;

/// A seat's `zwp_text_input_v3`. The seat holds it; a window it entered keeps a weak reference,
/// so a request from the window's thread holds the object alive for its length.
pub struct TextInput {
    text_input: Owned,
    /// The connection the object is on, which outlives it.
    _connection: Arc<Connection>,
}

impl std::fmt::Debug for TextInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TextInput").field(&self.text_input).finish()
    }
}

impl TextInput {
    /// `get_text_input` for `seat` (its registry name `id`), its events recorded in `inbox`.
    pub(crate) fn new(
        connection: Arc<Connection>,
        manager: Proxy,
        seat: Proxy,
        id: u32,
        inbox: &Inbox,
    ) -> Option<TextInput> {
        // SAFETY: the live manager and the seat `Seats` holds live (the loop's thread, between
        // dispatches).
        let proxy = unsafe { zwp_text_input_manager_v3::get_text_input(manager, seat) }?;
        // SAFETY: a text input made here, without a listener, owned by the value.
        let text_input = unsafe {
            Owned::listened(proxy, zwp_text_input_v3::destroy, inbox, move |_, opcode, args| {
                text_input_event(opcode, args).map(|event| Wire::TextInput { seat: id, event })
            })
        };
        Some(TextInput { text_input, _connection: connection })
    }

    /// Make a request on the live object.
    pub(crate) fn request(&self, request: impl FnOnce(Proxy)) {
        request(self.text_input.proxy())
    }

    pub(crate) fn enable(&self) {
        // SAFETY: the live text input.
        self.request(|p| unsafe { zwp_text_input_v3::enable(p) })
    }

    pub(crate) fn disable(&self) {
        // SAFETY: the live text input.
        self.request(|p| unsafe { zwp_text_input_v3::disable(p) })
    }

    pub(crate) fn commit(&self) {
        // SAFETY: the live text input.
        self.request(|p| unsafe { zwp_text_input_v3::commit(p) })
    }

    pub(crate) fn set_content_type_by_purpose(&self, purpose: ImePurpose) {
        use zwp_text_input_v3::{content_hint as hint, content_purpose as cp};
        let (hint, purpose) = match purpose {
            ImePurpose::Normal => (hint::NONE, cp::NORMAL),
            ImePurpose::Password => (hint::SENSITIVE_DATA, cp::PASSWORD),
            ImePurpose::Terminal => (hint::NONE, cp::TERMINAL),
        };
        // SAFETY: the live text input.
        self.request(|p| unsafe { zwp_text_input_v3::set_content_type(p, hint, purpose) })
    }
}

/// A `zwp_text_input_v3` event, decoded.
fn text_input_event(opcode: u32, args: &[EventArg<'_>]) -> Option<TextInputEvent> {
    use zwp_text_input_v3::evt;
    Some(match (opcode, args) {
        (evt::ENTER, [EventArg::Object(surface)]) => TextInputEvent::Enter(*surface),
        (evt::LEAVE, [EventArg::Object(surface)]) => TextInputEvent::Leave(*surface),
        (
            evt::PREEDIT_STRING,
            [EventArg::Str(text), EventArg::Int(cursor_begin), EventArg::Int(cursor_end)],
        ) => TextInputEvent::PreeditString {
            text: wire::text(*text),
            cursor_begin: *cursor_begin,
            cursor_end: *cursor_end,
        },
        (evt::COMMIT_STRING, [EventArg::Str(text)]) => {
            TextInputEvent::CommitString(wire::text(*text))
        },
        (evt::DONE, [EventArg::Uint(_)]) => TextInputEvent::Done,
        // `delete_surrounding_text`: not handled.
        _ => return None,
    })
}

impl WinitState {
    /// One event of `seat`'s text input.
    pub(crate) fn text_input_event(&mut self, seat: u32, event: TextInputEvent) {
        let Some((text_input, data)) =
            self.seats.get_mut(&seat).and_then(|seat_state| seat_state.text_input.as_mut())
        else {
            return;
        };
        let windows = self.windows.get_mut();
        match event {
            TextInputEvent::Enter(surface) => {
                let window_id = surface.map(wayland::make_wid);
                data.surface = window_id;

                let mut window = match window_id.and_then(|id| windows.get(&id)) {
                    Some(window) => window.lock().unwrap(),
                    None => return,
                };

                if window.ime_allowed() {
                    text_input.enable();
                    text_input.set_content_type_by_purpose(window.ime_purpose());
                    text_input.commit();
                    if let Some(window_id) = window_id {
                        self.events_sink
                            .push_window_event(WindowEvent::Ime(Ime::Enabled), window_id);
                    }
                }

                window.text_input_entered(text_input);
            },
            TextInputEvent::Leave(surface) => {
                data.surface = None;

                // Always issue a disable.
                text_input.disable();
                text_input.commit();

                // XXX this check is essential, because `leave` could have a
                // reference to nil surface...
                let Some(window_id) = surface.map(wayland::make_wid) else { return };
                let mut window = match windows.get(&window_id) {
                    Some(window) => window.lock().unwrap(),
                    None => return,
                };

                window.text_input_left(text_input);

                self.events_sink.push_window_event(WindowEvent::Ime(Ime::Disabled), window_id);
            },
            TextInputEvent::PreeditString { text, cursor_begin, cursor_end } => {
                let text = text.unwrap_or_default();
                let cursor_begin = usize::try_from(cursor_begin)
                    .ok()
                    .and_then(|idx| text.is_char_boundary(idx).then_some(idx));
                let cursor_end = usize::try_from(cursor_end)
                    .ok()
                    .and_then(|idx| text.is_char_boundary(idx).then_some(idx));

                data.pending_preedit = Some(Preedit { text, cursor_begin, cursor_end })
            },
            TextInputEvent::CommitString(text) => {
                data.pending_preedit = None;
                data.pending_commit = text;
            },
            TextInputEvent::Done => {
                let window_id = match data.surface {
                    Some(window_id) => window_id,
                    None => return,
                };

                // Clear preedit, unless all we'll be doing next is sending a new preedit.
                if data.pending_commit.is_some() || data.pending_preedit.is_none() {
                    self.events_sink.push_window_event(
                        WindowEvent::Ime(Ime::Preedit(String::new(), None)),
                        window_id,
                    );
                }

                // Send `Commit`.
                if let Some(text) = data.pending_commit.take() {
                    self.events_sink
                        .push_window_event(WindowEvent::Ime(Ime::Commit(text)), window_id);
                }

                // Send preedit.
                if let Some(preedit) = data.pending_preedit.take() {
                    let cursor_range =
                        preedit.cursor_begin.map(|b| (b, preedit.cursor_end.unwrap_or(b)));

                    self.events_sink.push_window_event(
                        WindowEvent::Ime(Ime::Preedit(preedit.text, cursor_range)),
                        window_id,
                    );
                }
            },
        }
    }
}

/// What a seat's text input said since its last `done`.
#[derive(Default)]
pub struct TextInputData {
    /// The window we're performing input to.
    surface: Option<WindowId>,

    /// The commit to submit on `done`.
    pending_commit: Option<String>,

    /// The preedit to submit on `done`.
    pending_preedit: Option<Preedit>,
}

/// The state of the preedit.
struct Preedit {
    text: String,
    cursor_begin: Option<usize>,
    cursor_end: Option<usize>,
}
