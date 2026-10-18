// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/seat/keyboard/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! The keyboard input handling.
//!
//! LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, LW.7): the keyboard's
//! events come decoded from first-party `aterm_wayland::seat` (its keymap read from the
//! compositor's descriptor), and key repeat is a deadline the event loop waits for in its own
//! poll (`EventLoop::poll_events_with_timeout`), where upstream registered a calloop timer.
//! The timing is calloop's: the first repeat `delay` after the press, then one every `gap`
//! from when the previous one fired.

use std::time::{Duration, Instant};

use aterm_wayland::protocols::wayland::wl_keyboard;
use aterm_wayland::seat::KeyboardEvent;
use tracing::warn;

use crate::event::{ElementState, WindowEvent};
use crate::keyboard::ModifiersState;

use crate::platform_impl::common::xkb::Context;
use crate::platform_impl::wayland::event_loop::sink::EventSink;
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

impl WinitState {
    /// One `wl_keyboard` event of `seat`.
    pub(crate) fn keyboard_event(&mut self, seat: u32, event: KeyboardEvent) {
        let seat_state = match self.seats.get_mut(&seat) {
            Some(seat_state) => seat_state,
            None => {
                warn!("Received keyboard event {event:?} without seat");
                return;
            },
        };
        // Where that keyboard IS: a clipboard copy needs it, because a compositor hands the
        // selection only to the client its keyboard is on (see `clipboard.rs`). Recorded before
        // the per-window bookkeeping below, which returns early on a surface we do not know —
        // mutter sends `leave` with a nil surface, and the keyboard has left us just the same.
        // (The serial a copy spends is aterm-wayland's seat's, which records each.)
        match &event {
            KeyboardEvent::Enter { .. } => seat_state.set_keyboard_entered(true),
            KeyboardEvent::Leave { .. } => seat_state.set_keyboard_entered(false),
            _ => {},
        }
        let keyboard_state = match seat_state.keyboard_state.as_mut() {
            Some(keyboard_state) => keyboard_state,
            None => {
                warn!("Received keyboard event {event:?} without keyboard");
                return;
            },
        };

        match event {
            KeyboardEvent::Keymap(Ok(keymap)) => match keymap.format {
                wl_keyboard::keymap_format::NO_KEYMAP => {
                    warn!("non-xkb compatible keymap")
                },
                wl_keyboard::keymap_format::XKB_V1 => {
                    keyboard_state.xkb_context.set_keymap_from_text(&keymap.text);
                },
                _value => {
                    warn!("unknown keymap format 0x{:x}", _value)
                },
            },
            KeyboardEvent::Keymap(Err(_error)) => {
                warn!("the compositor's keymap could not be read: {_error}");
            },
            KeyboardEvent::Enter { surface, .. } => {
                // A nil surface (one this client destroyed) is no window of ours.
                let Some(surface) = surface else { return };
                let window_id = wayland::make_wid(surface);

                // Mark the window as focused.
                let was_unfocused = match self.windows.get_mut().get(&window_id) {
                    Some(window) => {
                        let mut window = window.lock().unwrap();
                        let was_unfocused = !window.has_focus();
                        window.add_seat_focus(seat);
                        was_unfocused
                    },
                    None => return,
                };

                // Drop the repeat, if there were any.
                keyboard_state.stop_repeat();

                keyboard_state.window_id = Some(window_id);

                // The keyboard focus is considered as general focus.
                if was_unfocused {
                    self.events_sink.push_window_event(WindowEvent::Focused(true), window_id);
                }

                // HACK: this is just for GNOME not fixing their ordering issue of modifiers.
                if std::mem::take(&mut seat_state.modifiers_pending) {
                    self.events_sink.push_window_event(
                        WindowEvent::ModifiersChanged(seat_state.modifiers.into()),
                        window_id,
                    );
                }
            },
            KeyboardEvent::Leave { surface, .. } => {
                // NOTE: we should drop the repeat regardless whether it was for the present
                // window of for the window which just went gone.
                keyboard_state.stop_repeat();

                // NOTE: The check whether the window exists is essential as we might get a
                // nil surface, regardless of what protocol says.
                let Some(window_id) = surface.map(wayland::make_wid) else { return };
                let focused = match self.windows.get_mut().get(&window_id) {
                    Some(window) => {
                        let mut window = window.lock().unwrap();
                        window.remove_seat_focus(seat);
                        window.has_focus()
                    },
                    None => return,
                };

                // We don't need to update it above, because the next `Enter` will overwrite
                // anyway.
                keyboard_state.window_id = None;

                if !focused {
                    // Notify that no modifiers are being pressed.
                    self.events_sink.push_window_event(
                        WindowEvent::ModifiersChanged(ModifiersState::empty().into()),
                        window_id,
                    );

                    self.events_sink.push_window_event(WindowEvent::Focused(false), window_id);
                }
            },
            KeyboardEvent::Key { key, state: wl_keyboard::key_state::PRESSED, .. } => {
                let key = key + 8;

                key_input(keyboard_state, &mut self.events_sink, key, ElementState::Pressed, false);

                let delay = match keyboard_state.repeat_info {
                    RepeatInfo::Repeat { delay, .. } => delay,
                    RepeatInfo::Disable => return,
                };

                if !keyboard_state.xkb_context.keymap_mut().is_some_and(|k| k.key_repeats(key)) {
                    return;
                }

                // NOTE terminate ongoing timer and start a new timer.
                keyboard_state.current_repeat = Some(key);
                keyboard_state.repeat_deadline = Instant::now().checked_add(delay);
            },
            KeyboardEvent::Key { key, state: wl_keyboard::key_state::RELEASED, .. } => {
                let key = key + 8;

                key_input(
                    keyboard_state,
                    &mut self.events_sink,
                    key,
                    ElementState::Released,
                    false,
                );

                if keyboard_state.repeat_info != RepeatInfo::Disable
                    && keyboard_state.xkb_context.keymap_mut().is_some_and(|k| k.key_repeats(key))
                    && Some(key) == keyboard_state.current_repeat
                {
                    keyboard_state.stop_repeat();
                }
            },
            KeyboardEvent::Key { .. } => {},
            KeyboardEvent::Modifiers { depressed, latched, locked, group, .. } => {
                let xkb_context = &mut keyboard_state.xkb_context;
                let xkb_state = match xkb_context.state_mut() {
                    Some(state) => state,
                    None => return,
                };

                xkb_state.update_modifiers(depressed, latched, locked, 0, 0, group);
                seat_state.modifiers = xkb_state.modifiers().into();

                // HACK: part of the workaround from `WlKeyboardEvent::Enter`.
                let window_id = match keyboard_state.window_id {
                    Some(window_id) => window_id,
                    None => {
                        seat_state.modifiers_pending = true;
                        return;
                    },
                };

                self.events_sink.push_window_event(
                    WindowEvent::ModifiersChanged(seat_state.modifiers.into()),
                    window_id,
                );
            },
            KeyboardEvent::RepeatInfo { rate, delay } => {
                keyboard_state.repeat_info = if rate <= 0 {
                    // Stop the repeat once we get a disable event.
                    keyboard_state.stop_repeat();
                    RepeatInfo::Disable
                } else {
                    let gap = Duration::from_micros(1_000_000 / rate as u64);
                    let delay = Duration::from_millis(delay.max(0) as u64);
                    RepeatInfo::Repeat { gap, delay }
                };
            },
        }
    }

    /// The earliest moment a held key repeats, across every seat.
    pub(crate) fn next_key_repeat(&self) -> Option<Instant> {
        self.seats.values().filter_map(|seat| seat.keyboard_state.as_ref()?.repeat_deadline).min()
    }

    /// Repeat each held key whose deadline has come, once, and schedule its next repeat a
    /// `gap` from now (calloop's `TimeoutAction::ToDuration`). Whether any repeated.
    pub(crate) fn fire_key_repeats(&mut self, now: Instant) -> bool {
        let mut fired = false;
        for seat_state in self.seats.values_mut() {
            let Some(keyboard_state) = seat_state.keyboard_state.as_mut() else {
                continue;
            };
            if keyboard_state.repeat_deadline.is_none_or(|deadline| deadline > now) {
                continue;
            }
            // NOTE: The removed on event source is batched, but key change to `None` is
            // instant.
            let Some(repeat_keycode) = keyboard_state.current_repeat else {
                keyboard_state.repeat_deadline = None;
                continue;
            };
            // Required to handle the wakeups from the repeat sources.
            fired = true;
            key_input(
                keyboard_state,
                &mut self.events_sink,
                repeat_keycode,
                ElementState::Pressed,
                true,
            );
            // NOTE: the gap could change dynamically while repeat is going.
            keyboard_state.repeat_deadline = match keyboard_state.repeat_info {
                RepeatInfo::Repeat { gap, .. } => Instant::now().checked_add(gap),
                RepeatInfo::Disable => None,
            };
        }
        fired
    }
}

/// The state of the keyboard on the current seat.
#[derive(Debug)]
pub struct KeyboardState {
    /// The state of the keyboard.
    pub xkb_context: Context,

    /// The information about the repeat rate obtained from the compositor.
    pub repeat_info: RepeatInfo,

    /// When the held key repeats next.
    pub repeat_deadline: Option<Instant>,

    /// The current repeat raw key.
    pub current_repeat: Option<u32>,

    /// The currently focused window surface. Could be `None` on bugged compositors, like mutter.
    window_id: Option<WindowId>,
}

impl KeyboardState {
    /// A keyboard's state; `None` (with a warning) without libxkbcommon.
    pub fn new() -> Option<Self> {
        match Context::new() {
            Ok(xkb_context) => Some(Self {
                xkb_context,
                repeat_info: RepeatInfo::default(),
                repeat_deadline: None,
                current_repeat: None,
                window_id: None,
            }),
            Err(_error) => {
                warn!("the keyboard has no xkb context: {_error:?}");
                None
            },
        }
    }

    /// Drop the repeat, if there is one.
    fn stop_repeat(&mut self) {
        self.current_repeat = None;
        self.repeat_deadline = None;
    }
}

/// The rate at which a pressed key is repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatInfo {
    /// Keys will be repeated at the specified rate and delay.
    Repeat {
        /// The time between the key repeats.
        gap: Duration,

        /// Delay (in milliseconds) between a key press and the start of repetition.
        delay: Duration,
    },

    /// Keys should not be repeated.
    Disable,
}

impl Default for RepeatInfo {
    /// The default repeat rate is 25 keys per second with the delay of 200ms.
    ///
    /// The values are picked based on the default in various compositors and Xorg.
    fn default() -> Self {
        Self::Repeat { gap: Duration::from_millis(40), delay: Duration::from_millis(200) }
    }
}

fn key_input(
    keyboard_state: &mut KeyboardState,
    event_sink: &mut EventSink,
    keycode: u32,
    state: ElementState,
    repeat: bool,
) {
    let window_id = match keyboard_state.window_id {
        Some(window_id) => window_id,
        None => return,
    };

    let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
    if let Some(mut key_context) = keyboard_state.xkb_context.key_context() {
        let event = key_context.process_key_event(keycode, state, repeat);
        let event = WindowEvent::KeyboardInput { device_id, event, is_synthetic: false };
        event_sink.push_window_event(event, window_id);
    }
}
