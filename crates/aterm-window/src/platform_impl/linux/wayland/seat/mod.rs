// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/seat/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Seat handling.
//!
//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, LW.7 with winit-core
//! WC.8): the seats are first-party `aterm_wayland::seat::Seats`, which binds every `wl_seat` (at
//! version 7, as smithay-client-toolkit did), makes and releases its devices as its capabilities
//! come and go (in sctk's order) and records their events in the backend's one inbox as they
//! are heard (`wire::Inbox::seat_sink`); this module keeps what winit keeps per seat and applies
//! those events where the inbox has them, as sctk's `SeatHandler` and the device handlers did.
//! CUT with sctk, each with no aterm caller: the relative pointer (`DeviceEvent::MouseMotion` and
//! the motion axes) and pointer constraints.

use std::sync::Arc;

use aterm_hash::FxHashMap;
use aterm_wayland::protocols::wayland::wl_seat;
use aterm_wayland::seat::Input;
use aterm_wayland::selection::{Device, Manager};
use tracing::warn;

use crate::event::WindowEvent;
use crate::keyboard::ModifiersState;
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::wire::Inbox;

pub(crate) mod data_device;
mod keyboard;
mod pointer;
mod text_input;
mod touch;

pub(crate) use pointer::WinitPointer;
pub(crate) use text_input::{TextInput, TextInputData};

use keyboard::KeyboardState;
use touch::TouchPoint;

#[derive(Default)]
pub struct WinitSeatState {
    /// The capabilities the seat last announced (`wl_seat.capability` bits).
    capabilities: u32,

    /// The pointer bound on the seat, with its cursor.
    pointer: Option<Arc<WinitPointer>>,

    /// Whether the seat has a touch device.
    touch: bool,

    /// The mapping from touched points to the surfaces they're present.
    touch_map: FxHashMap<i32, TouchPoint>,

    /// The text input bound on the seat, and what it has said since its last `done`.
    text_input: Option<(Arc<TextInput>, TextInputData)>,

    /// The keyboard bound on the seat.
    keyboard_state: Option<KeyboardState>,

    /// The data device bound on the seat, driving drag-and-drop (and clipboard)
    /// offers. `None` when the compositor has no `wl_data_device_manager`. Kept
    /// alive here so its offer events keep flowing (see `data_device.rs`). Made
    /// when the seat is first seen (see [`WinitState::sync_seats`]).
    pub(crate) data_device: Option<Device>,

    /// The primary-selection device bound on the seat (see `clipboard.rs`).
    /// `None` when the compositor has no `zwp_primary_selection_device_manager_v1`.
    pub(crate) primary_device: Option<Device>,

    /// Whether the seat's keyboard is currently ON one of this client's surfaces
    /// — set by `wl_keyboard.enter`, cleared by `wl_keyboard.leave`. Read by
    /// [`Self::holds_keyboard_focus`]; see `clipboard.rs` for why a copy needs it.
    keyboard_entered: bool,

    /// The current modifiers state on the seat.
    modifiers: ModifiersState,

    /// Whether we have pending modifiers.
    modifiers_pending: bool,
}

impl WinitSeatState {
    /// A seat first seen (its registry name `id`): its data devices, where the compositor
    /// offers their managers, their drags recorded in `inbox`.
    fn new(
        id: u32,
        seat: aterm_wayland::proxy::Proxy,
        data: Option<&Manager>,
        primary: Option<&Manager>,
        inbox: &Inbox,
    ) -> Self {
        // SAFETY: the seat aterm-wayland's `Seats` holds live; its devices are dropped before it
        // is released (`WinitState::sync_seats` drops this state first).
        let device = |manager: Option<&Manager>| {
            manager.and_then(|manager| {
                match unsafe { Device::new(manager, seat, inbox.drag_sink(id)) } {
                    Ok(device) => Some(device),
                    Err(_error) => {
                        warn!("wayland: a seat has no data device: {_error}");
                        None
                    },
                }
            })
        };
        Self { data_device: device(data), primary_device: device(primary), ..Default::default() }
    }

    /// Whether this client holds the seat's keyboard focus — the fact a clipboard
    /// copy is gated on, because a compositor hands a selection only to the client
    /// its keyboard is on (measured; see `clipboard.rs`).
    ///
    /// A seat that has bound NO keyboard answers `true`: there is no focus on it to
    /// hold, and a gate that read `false` there would refuse every copy made on a
    /// pointer-only seat.
    pub(crate) fn holds_keyboard_focus(&self) -> bool {
        self.keyboard_state.is_none() || self.keyboard_entered
    }

    /// `wl_keyboard.enter` / `.leave` for this seat: the keyboard arrived on one of
    /// our surfaces, or left them.
    pub(crate) fn set_keyboard_entered(&mut self, entered: bool) {
        self.keyboard_entered = entered;
    }
}

/// A seat's pointer or text input after its capability (or its seat) went. A window keeps a weak
/// reference to each and upgrades it for a request on whatever thread the application calls it
/// from; were that thread's reference the last, the object would be destroyed, and its listener
/// freed, there, while the loop's thread may be dispatching its events (wayland-rs kept an
/// object's data alive across its dispatch; libwayland and aterm-wayland do not). So the loop
/// keeps its reference here and lets go only once it is the last one
/// ([`WinitState::reap_retired`]): the object is destroyed on the loop's thread, between
/// dispatches.
pub(crate) enum Retired {
    Pointer(Arc<WinitPointer>),
    TextInput(Arc<TextInput>),
}

/// One `wl_seat.capability` bit, in the order sctk 0.19.2 handled them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeatCapability {
    Keyboard,
    Pointer,
    Touch,
}

impl SeatCapability {
    const ORDER: [SeatCapability; 3] =
        [SeatCapability::Keyboard, SeatCapability::Pointer, SeatCapability::Touch];

    fn bit(self) -> u32 {
        match self {
            SeatCapability::Keyboard => wl_seat::capability::KEYBOARD,
            SeatCapability::Pointer => wl_seat::capability::POINTER,
            SeatCapability::Touch => wl_seat::capability::TOUCH,
        }
    }
}

impl WinitState {
    /// Bring the seats up to the registry: a seat the registry no longer lists is torn down
    /// here first (its devices before aterm-wayland releases it), then `Seats` binds the new
    /// ones, and each seat first seen gets its data devices (sctk's `new_seat`).
    pub(crate) fn sync_seats(&mut self) {
        let listed: Vec<u32> = self
            .registry()
            .globals()
            .into_iter()
            .filter(|g| g.interface.as_c_str() == c"wl_seat")
            .map(|g| g.name)
            .collect();
        let gone: Vec<u32> =
            self.seats.keys().copied().filter(|seat| !listed.contains(seat)).collect();
        for seat in gone {
            // sctk's `remove_seat`; the seat's pointer and text input are retired, as a removed
            // capability's are.
            if let Some(mut seat_state) = self.seats.remove(&seat) {
                if let Some((text_input, _)) = seat_state.text_input.take() {
                    self.retired.push(Retired::TextInput(text_input));
                }
                if let Some(pointer) = seat_state.pointer.take() {
                    self.retire_pointer(pointer);
                }
            }
            self.on_keyboard_destroy(seat);
        }
        if let Err(_error) = self.seats_book.sync(self.registry()) {
            warn!("wayland: the seats could not be bound: {_error}");
        }
        self.add_new_seats();
    }

    /// A [`WinitSeatState`] for each seat `Seats` holds that has none yet.
    pub(crate) fn add_new_seats(&mut self) {
        for info in self.seats_book.infos() {
            if self.seats.contains_key(&info.id) {
                continue;
            }
            let Some(proxy) = self.seats_book.proxy(info.id) else {
                continue;
            };
            let seat = WinitSeatState::new(
                info.id,
                proxy,
                self.data_device_manager.as_ref(),
                self.primary_selection_manager.as_ref(),
                &self.inbox,
            );
            self.seats.insert(info.id, seat);
        }
    }

    /// One seat's input, where the inbox has it.
    pub(crate) fn seat_input(&mut self, input: Input) {
        match input {
            Input::Capabilities { seat, capabilities } => {
                self.capabilities_changed(seat, capabilities)
            },
            Input::Pointer { seat, frame } => self.pointer_frame(seat, &frame),
            Input::Keyboard { seat, event } => self.keyboard_event(seat, event),
            Input::Touch { seat, event } => self.touch_event(seat, event),
        }
    }

    /// `wl_seat.capabilities`: each capability that came or went, as sctk 0.19.2 told
    /// winit's `SeatHandler` (keyboard, pointer, touch).
    fn capabilities_changed(&mut self, seat: u32, capabilities: u32) {
        let Some(old) = self.seats.get(&seat).map(|s| s.capabilities) else {
            warn!("Received wl_seat::capabilities for unknown seat");
            return;
        };
        if let Some(seat_state) = self.seats.get_mut(&seat) {
            seat_state.capabilities = capabilities;
        }
        for capability in SeatCapability::ORDER {
            let (had, has) = (old & capability.bit() != 0, capabilities & capability.bit() != 0);
            match (had, has) {
                (false, true) => self.new_capability(seat, capability),
                (true, false) => self.remove_capability(seat, capability),
                _ => (),
            }
        }
    }

    fn new_capability(&mut self, seat: u32, capability: SeatCapability) {
        let compositor = self.compositor();
        let cursor_shape_manager = self.cursor_shape_manager.as_ref().map(|m| m.proxy());
        let shm = self.shm();
        let pointer_proxy = self.seats_book.pointer(seat);
        let seat_proxy = self.seats_book.proxy(seat);
        let inbox = self.inbox.clone();
        let connection = Arc::clone(&self.connection);
        let Some(seat_state) = self.seats.get_mut(&seat) else {
            warn!("Received wl_seat::new_capability for unknown seat");
            return;
        };

        match capability {
            SeatCapability::Touch => seat_state.touch = true,
            SeatCapability::Keyboard if seat_state.keyboard_state.is_none() => {
                seat_state.keyboard_state = KeyboardState::new();
            },
            SeatCapability::Pointer if seat_state.pointer.is_none() => {
                let pointer = pointer_proxy.and_then(|pointer_proxy| {
                    WinitPointer::new(
                        seat,
                        Arc::clone(&connection),
                        compositor,
                        shm,
                        pointer_proxy,
                        cursor_shape_manager,
                        &inbox,
                    )
                });
                if let Some(pointer) = pointer {
                    let pointer = Arc::new(pointer);
                    // Register cursor surface.
                    self.pointer_surfaces.insert(pointer.surface_id(), pointer.clone());
                    seat_state.pointer = Some(pointer);
                } else {
                    warn!("failed to create pointer with present capability.");
                }
            },
            _ => (),
        }

        if seat_state.text_input.is_none() {
            if let (Some(manager), Some(seat_proxy)) =
                (self.text_input_manager.as_ref(), seat_proxy)
            {
                seat_state.text_input =
                    TextInput::new(connection, manager.proxy(), seat_proxy, seat, &inbox)
                        .map(|text_input| (Arc::new(text_input), TextInputData::default()));
            }
        }
    }

    fn remove_capability(&mut self, seat: u32, capability: SeatCapability) {
        let Some(seat_state) = self.seats.get_mut(&seat) else {
            warn!("Received wl_seat::remove_capability for unknown seat");
            return;
        };

        // The text input goes with any capability (a window keeps only a weak reference; it is
        // destroyed on this thread once no window holds it).
        if let Some((text_input, _)) = seat_state.text_input.take() {
            self.retired.push(Retired::TextInput(text_input));
        }

        match capability {
            SeatCapability::Touch => {
                seat_state.touch = false;
                seat_state.touch_map.clear();
            },
            SeatCapability::Pointer => {
                if let Some(pointer) = seat_state.pointer.take() {
                    self.retire_pointer(pointer);
                }
            },
            SeatCapability::Keyboard => {
                seat_state.keyboard_state = None;
                // The keyboard is gone, so it is on none of our surfaces; a later
                // one re-binds and re-enters before it hands out another serial.
                seat_state.set_keyboard_entered(false);
                self.on_keyboard_destroy(seat);
            },
        }
    }

    /// A pointer whose capability or seat went: out of the cursor mapping, then retired.
    fn retire_pointer(&mut self, pointer: Arc<WinitPointer>) {
        let _ = self.pointer_surfaces.remove(&pointer.surface_id());
        let _ = self.surface_scales.remove(&pointer.surface_id());
        self.retired.push(Retired::Pointer(pointer));
    }

    /// Destroy the retired objects no window holds any more (between dispatches, on the loop's
    /// thread).
    pub(crate) fn reap_retired(&mut self) {
        for retired in std::mem::take(&mut self.retired) {
            // `try_unwrap` succeeds only for the last reference, and a window's weak reference
            // can no longer be upgraded once it has; the object is dropped here when it does.
            let kept = match retired {
                Retired::Pointer(pointer) => Arc::try_unwrap(pointer).err().map(Retired::Pointer),
                Retired::TextInput(text_input) => {
                    Arc::try_unwrap(text_input).err().map(Retired::TextInput)
                },
            };
            self.retired.extend(kept);
        }
    }

    fn on_keyboard_destroy(&mut self, seat: u32) {
        for (window_id, window) in self.windows.get_mut() {
            let mut window = window.lock().unwrap();
            let had_focus = window.has_focus();
            window.remove_seat_focus(seat);
            if had_focus != window.has_focus() {
                self.events_sink.push_window_event(WindowEvent::Focused(false), *window_id);
            }
        }
    }
}
