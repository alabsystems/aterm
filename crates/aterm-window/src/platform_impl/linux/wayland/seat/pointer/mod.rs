// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/seat/pointer/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Wayland pointer event handling and cursor management.

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use aterm_wayland::cursor::Theme;
use aterm_wayland::pointer_frame::{PointerEvent, PointerKind};
use aterm_wayland::protocols::cursor_shape_v1::{
    wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1,
};
use aterm_wayland::protocols::wayland::{wl_compositor, wl_pointer, wl_surface};
use aterm_wayland::proxy::Proxy;
use aterm_wayland::seat::Seats;
use tracing::warn;

use crate::dpi::{LogicalPosition, PhysicalPosition};
use crate::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use crate::window::{CursorIcon, ResizeDirection};

use crate::platform_impl::wayland::state::{Connection, WinitState};
use crate::platform_impl::wayland::types::cursor::{shape, theme_names};
use crate::platform_impl::wayland::wire::{self, Inbox, Owned, Wire};
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

impl WinitState {
    /// One frame of `seat`'s pointer (sctk's `PointerHandler::pointer_frame`).
    pub(crate) fn pointer_frame(&mut self, seat: u32, events: &[PointerEvent<Proxy>]) {
        let seat_state = match self.seats.get(&seat) {
            Some(seat_state) => seat_state,
            None => {
                warn!("Received pointer event without seat");
                return;
            },
        };

        let themed_pointer = match seat_state.pointer.as_ref() {
            Some(pointer) => pointer.clone(),
            None => {
                warn!("Received pointer event without pointer");
                return;
            },
        };

        let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));

        for event in events {
            let window_id = wayland::make_wid(event.surface);

            let mut window = match self.windows.get_mut().get_mut(&window_id) {
                Some(window) => window.lock().unwrap(),
                None => {
                    self.frame_pointer_event(seat, &themed_pointer, window_id, event);
                    continue;
                },
            };

            let scale_factor = window.scale_factor();
            let position: PhysicalPosition<f64> =
                LogicalPosition::new(event.position.0, event.position.1).to_physical(scale_factor);

            match event.kind {
                PointerKind::Enter { .. } => {
                    self.events_sink
                        .push_window_event(WindowEvent::CursorEntered { device_id }, window_id);

                    window.pointer_entered(Arc::downgrade(&themed_pointer));

                    themed_pointer.set_focused_window(Some(window_id));

                    self.events_sink.push_window_event(
                        WindowEvent::CursorMoved { device_id, position },
                        window_id,
                    );
                },
                PointerKind::Leave { .. } => {
                    window.pointer_left(&Arc::downgrade(&themed_pointer));

                    themed_pointer.set_focused_window(None);

                    self.events_sink
                        .push_window_event(WindowEvent::CursorLeft { device_id }, window_id);
                },
                PointerKind::Motion { .. } => {
                    self.events_sink.push_window_event(
                        WindowEvent::CursorMoved { device_id, position },
                        window_id,
                    );
                },
                ref kind @ PointerKind::Press { button, .. }
                | ref kind @ PointerKind::Release { button, .. } => {
                    let button = wayland_button_to_winit(button);
                    let state = if matches!(kind, PointerKind::Press { .. }) {
                        ElementState::Pressed
                    } else {
                        ElementState::Released
                    };
                    self.events_sink.push_window_event(
                        WindowEvent::MouseInput { device_id, state, button },
                        window_id,
                    );
                },
                PointerKind::Axis { horizontal, vertical, .. } => {
                    let has_discrete_scroll = horizontal.discrete != 0 || vertical.discrete != 0;

                    let phase = if horizontal.stop || vertical.stop {
                        TouchPhase::Ended
                    } else {
                        match themed_pointer.phase() {
                            _ if has_discrete_scroll => TouchPhase::Moved,
                            TouchPhase::Started | TouchPhase::Moved => TouchPhase::Moved,
                            _ => TouchPhase::Started,
                        }
                    };

                    themed_pointer.set_phase(phase);

                    let delta = if has_discrete_scroll {
                        MouseScrollDelta::LineDelta(
                            (-horizontal.discrete) as f32,
                            (-vertical.discrete) as f32,
                        )
                    } else {
                        MouseScrollDelta::PixelDelta(
                            LogicalPosition::new(-horizontal.absolute, -vertical.absolute)
                                .to_physical(scale_factor),
                        )
                    };

                    self.events_sink.push_window_event(
                        WindowEvent::MouseWheel { device_id, delta, phase },
                        window_id,
                    )
                },
            }
        }
    }
}

impl WinitState {
    /// `event` of `seat`'s pointer on `surface`, which is no window: if it is a part of a
    /// window's shadow margin, the resize cursor of the edge or corner the pointer is near, and
    /// a left press there resizes the window by it.
    fn frame_pointer_event(
        &mut self,
        seat: u32,
        pointer: &Arc<WinitPointer>,
        surface: WindowId,
        event: &PointerEvent<Proxy>,
    ) {
        let owner = self.windows.get_mut().values().find_map(|window| {
            let part = window.lock().unwrap().frame_part(surface)?;
            Some((Arc::clone(window), part))
        });
        let Some((window, part)) = owner else {
            return;
        };
        let window = window.lock().unwrap();
        let (x, y) = event.position;
        match event.kind {
            PointerKind::Enter { .. } | PointerKind::Motion { .. } => {
                let direction = window.frame_direction(part, x, y);
                if pointer.set_frame_edge(direction) {
                    let icon = direction.map_or(CursorIcon::Default, resize_icon);
                    if !pointer.set_cursor(&self.seats_book, icon) {
                        warn!("Failed to set cursor to {icon:?}");
                    }
                }
            },
            PointerKind::Leave { .. } => {
                pointer.set_frame_edge(None);
            },
            PointerKind::Press { button, serial, .. }
                if wayland_button_to_winit(button) == MouseButton::Left =>
            {
                if let Some(direction) = window.frame_direction(part, x, y) {
                    window.resize_from_seat(seat, serial, direction);
                }
            },
            _ => (),
        }
    }
}

/// The cursor that shows a resize by `direction`.
fn resize_icon(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North => CursorIcon::NResize,
        ResizeDirection::NorthEast => CursorIcon::NeResize,
        ResizeDirection::East => CursorIcon::EResize,
        ResizeDirection::SouthEast => CursorIcon::SeResize,
        ResizeDirection::South => CursorIcon::SResize,
        ResizeDirection::SouthWest => CursorIcon::SwResize,
        ResizeDirection::West => CursorIcon::WResize,
        ResizeDirection::NorthWest => CursorIcon::NwResize,
    }
}

/// What the pointer's frames keep between them.
#[derive(Debug)]
struct PointerFocus {
    /// Currently focused window.
    surface: Option<WindowId>,
    /// Current axis phase.
    phase: TouchPhase,
    /// The edge or corner a shadow-margin part under the pointer resizes by, as its cursor
    /// shows it (`None`: on no part, or one that resizes nothing).
    frame_edge: Option<ResizeDirection>,
}

/// A seat's pointer state, scroll phase, and cursor device or surface.
pub struct WinitPointer {
    /// The seat (its registry name).
    seat: u32,
    /// The cursor surface's integer scale (sctk's `SurfaceData::scale_factor`).
    cursor_scale: AtomicI32,
    focus: Mutex<PointerFocus>,
    /// The loaded cursor themes, one per scale, and their buffers.
    themes: Mutex<Vec<(i32, Theme)>>,
    /// `wp_cursor_shape_device_v1`, where the compositor offers cursor-shape-v1.
    shape_device: Option<Owned>,
    /// The surface the theme's images are shown on.
    surface: Owned,
    /// Keep the theme's global alive through a window thread's last in-flight
    /// request, including one that outlasts the event loop.
    shm: Arc<Owned>,
    /// The connection the objects are on, which outlives them.
    _connection: Arc<Connection>,
}

impl WinitPointer {
    /// The cursor objects for `seat`'s live `pointer`.
    pub(crate) fn new(
        seat: u32,
        connection: Arc<Connection>,
        compositor: Proxy,
        shm: Arc<Owned>,
        pointer: Proxy,
        cursor_shape_manager: Option<Proxy>,
        inbox: &Inbox,
    ) -> Option<WinitPointer> {
        // SAFETY: the live compositor; the surface is this value's.
        let surface = unsafe { wl_compositor::create_surface(compositor) }?;
        let surface_id = wayland::make_wid(surface);
        // SAFETY: a surface made here, without a listener, owned by the pointer.
        let surface = unsafe {
            Owned::listened(surface, wl_surface::destroy, inbox, move |_, opcode, args| {
                wire::surface_event(opcode, args)
                    .map(|event| Wire::Surface { surface: surface_id, event })
            })
        };
        let shape_device = cursor_shape_manager.and_then(|manager| {
            // SAFETY: the live manager and the seat's live pointer (the loop's thread, between
            // dispatches: `Seats` releases devices only while dispatching).
            let device = unsafe { wp_cursor_shape_manager_v1::get_pointer(manager, pointer) }?;
            // SAFETY: a device made here, without events.
            Some(unsafe { Owned::new(device, wp_cursor_shape_device_v1::destroy) })
        });
        Some(WinitPointer {
            seat,
            shm,
            cursor_scale: AtomicI32::new(1),
            focus: Mutex::new(PointerFocus {
                surface: None,
                phase: TouchPhase::Ended,
                frame_edge: None,
            }),
            themes: Mutex::new(Vec::new()),
            shape_device,
            surface,
            _connection: connection,
        })
    }

    /// The seat's registry name.
    pub(crate) fn seat(&self) -> u32 {
        self.seat
    }

    /// The cursor surface's id (its key in the backend's surface maps).
    pub(crate) fn surface_id(&self) -> WindowId {
        wayland::make_wid(self.surface.proxy())
    }

    /// The window the pointer is on.
    pub(crate) fn focused_window(&self) -> Option<WindowId> {
        self.focus.lock().unwrap_or_else(PoisonError::into_inner).surface
    }

    fn set_focused_window(&self, window: Option<WindowId>) {
        self.focus.lock().unwrap_or_else(PoisonError::into_inner).surface = window;
    }

    fn phase(&self) -> TouchPhase {
        self.focus.lock().unwrap_or_else(PoisonError::into_inner).phase
    }

    fn set_phase(&self, phase: TouchPhase) {
        self.focus.lock().unwrap_or_else(PoisonError::into_inner).phase = phase;
    }

    /// Note the edge a shadow-margin part under the pointer resizes by; true when it moved.
    fn set_frame_edge(&self, edge: Option<ResizeDirection>) -> bool {
        let mut focus = self.focus.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::replace(&mut focus.frame_edge, edge) != edge
    }

    /// The cursor surface's integer scale changed.
    pub(crate) fn set_cursor_scale(&self, scale: i32) {
        self.cursor_scale.store(scale.max(1), Ordering::Relaxed);
    }

    /// Sets the pointer cursor to `icon` via cursor-shape-v1 or the legacy cursor theme.
    pub(crate) fn set_cursor(&self, seats: &Seats, icon: CursorIcon) -> bool {
        if let Some(device) = self.shape_device.as_ref() {
            return seats
                .with_seat(self.seat, |seat| {
                    let Some(serial) = seat.pointer_enter_serial else { return false };
                    // SAFETY: the pointer's live shape device.
                    unsafe {
                        wp_cursor_shape_device_v1::set_shape(device.proxy(), serial, shape(icon))
                    };
                    true
                })
                .unwrap_or(false);
        }
        self.set_cursor_legacy(seats, icon)
    }

    /// The legacy method of loading the cursor from the system cursor theme instead of relying
    /// on compositor to set the cursor.
    fn set_cursor_legacy(&self, seats: &Seats, icon: CursorIcon) -> bool {
        let scale = self.cursor_scale.load(Ordering::Relaxed);
        let names = theme_names(icon);
        let names: Vec<&CStr> = names.iter().map(CString::as_c_str).collect();
        let frame = {
            let mut themes = self.themes.lock().unwrap_or_else(PoisonError::into_inner);
            if !themes.iter().any(|(s, _)| *s == scale) {
                match load_theme(scale, self.shm.proxy()) {
                    Some(theme) => themes.push((scale, theme)),
                    None => return false,
                }
            }
            themes
                .iter()
                .find(|(s, _)| *s == scale)
                .and_then(|(_, theme)| theme.frame_named(&names))
        };
        let Some(image) = frame else {
            return false;
        };
        let surface = self.surface.proxy();
        // SAFETY: the pointer's live cursor surface and the theme's live buffer (the theme is
        // kept until the pointer goes).
        unsafe {
            if surface.version() >= 3 {
                wl_surface::set_buffer_scale(surface, scale);
            }
            wl_surface::attach(surface, Some(image.buffer), 0, 0);
            if surface.version() >= 4 {
                wl_surface::damage_buffer(surface, 0, 0, image.width as i32, image.height as i32);
            } else {
                wl_surface::damage(
                    surface,
                    0,
                    0,
                    image.width as i32 / scale,
                    image.height as i32 / scale,
                );
            }
            wl_surface::commit(surface);
        }
        let (hx, hy) = (image.hotspot_x as i32 / scale, image.hotspot_y as i32 / scale);
        seats
            .with_seat(self.seat, |seat| {
                let (Some(pointer), Some(serial)) = (seat.pointer, seat.pointer_enter_serial)
                else {
                    return false;
                };
                // SAFETY: the seat's pointer, lent live, and the live cursor surface.
                unsafe { wl_pointer::set_cursor(pointer, serial, Some(surface), hx, hy) };
                true
            })
            .unwrap_or(false)
    }
}

/// The user's cursor theme (`XCURSOR_THEME`, else `default`) at `XCURSOR_SIZE` (else 24) times
/// `scale`, as sctk's `ThemeSpec::System` loaded it.
fn load_theme(scale: i32, shm: Proxy) -> Option<Theme> {
    let name = std::env::var("XCURSOR_THEME").ok().unwrap_or_else(|| "default".into());
    let size: u32 = std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
    let name = CString::new(name).ok()?;
    let size = size.saturating_mul(u32::try_from(scale).unwrap_or(1));
    // SAFETY: the pointer retains the backend's `wl_shm` owner through this call and
    // every theme; its themes are destroyed before that retained owner.
    match unsafe { Theme::load(Some(&name), size, shm) } {
        Ok(theme) => Some(theme),
        Err(_error) => {
            warn!("the cursor theme could not be loaded: {_error}");
            None
        },
    }
}

/// Convert the Wayland button into winit.
fn wayland_button_to_winit(button: u32) -> MouseButton {
    const BTN_LEFT: u32 = 0x110;
    const BTN_RIGHT: u32 = 0x111;
    const BTN_MIDDLE: u32 = 0x112;
    const BTN_SIDE: u32 = 0x113;
    const BTN_EXTRA: u32 = 0x114;
    const BTN_FORWARD: u32 = 0x115;
    const BTN_BACK: u32 = 0x116;

    match button {
        BTN_LEFT => MouseButton::Left,
        BTN_RIGHT => MouseButton::Right,
        BTN_MIDDLE => MouseButton::Middle,
        BTN_BACK | BTN_SIDE => MouseButton::Back,
        BTN_FORWARD | BTN_EXTRA => MouseButton::Forward,
        button => MouseButton::Other(button as u16),
    }
}
