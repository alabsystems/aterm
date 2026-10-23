// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/seat/touch/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Touch handling.

use aterm_wayland::seat::TouchEvent;
use tracing::warn;

use crate::dpi::LogicalPosition;
use crate::event::{Touch, TouchPhase, WindowEvent};

use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

impl WinitState {
    /// One `wl_touch` event of `seat`.
    pub(crate) fn touch_event(&mut self, seat: u32, event: TouchEvent) {
        match event {
            TouchEvent::Down { surface, id, x, y, .. } => {
                self.touch_down(seat, surface, id, (x, y))
            },
            TouchEvent::Up { id, .. } => self.touch_up(seat, id),
            TouchEvent::Motion { id, x, y, .. } => self.touch_motion(seat, id, (x, y)),
            TouchEvent::Cancel => self.touch_cancel(seat),
            TouchEvent::Frame | TouchEvent::Shape { .. } | TouchEvent::Orientation { .. } => {},
        }
    }

    fn touch_down(
        &mut self,
        seat: u32,
        surface: Option<aterm_wayland::proxy::Proxy>,
        id: i32,
        position: (f64, f64),
    ) {
        let Some(window_id) = surface.map(wayland::make_wid) else { return };
        let scale_factor = match self.windows.get_mut().get(&window_id) {
            Some(window) => window.lock().unwrap().scale_factor(),
            None => return,
        };

        let seat_state = match self.seats.get_mut(&seat) {
            Some(seat_state) => seat_state,
            None => {
                warn!("Received wl_touch::down without seat");
                return;
            },
        };

        let location = LogicalPosition::<f64>::from(position);
        seat_state.touch_map.insert(id, TouchPoint { window_id, location });

        self.events_sink.push_window_event(
            WindowEvent::Touch(Touch {
                device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(
                    DeviceId,
                )),
                phase: TouchPhase::Started,
                location: location.to_physical(scale_factor),
                force: None,
                id: id as u64,
            }),
            window_id,
        );
    }

    fn touch_up(&mut self, seat: u32, id: i32) {
        let seat_state = match self.seats.get_mut(&seat) {
            Some(seat_state) => seat_state,
            None => {
                warn!("Received wl_touch::up without seat");
                return;
            },
        };

        let touch_point = match seat_state.touch_map.remove(&id) {
            Some(touch_point) => touch_point,
            None => return,
        };

        let window_id = touch_point.window_id;
        let scale_factor = match self.windows.get_mut().get(&window_id) {
            Some(window) => window.lock().unwrap().scale_factor(),
            None => return,
        };

        self.events_sink.push_window_event(
            WindowEvent::Touch(Touch {
                device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(
                    DeviceId,
                )),
                phase: TouchPhase::Ended,
                location: touch_point.location.to_physical(scale_factor),
                force: None,
                id: id as u64,
            }),
            window_id,
        );
    }

    fn touch_motion(&mut self, seat: u32, id: i32, position: (f64, f64)) {
        let seat_state = match self.seats.get_mut(&seat) {
            Some(seat_state) => seat_state,
            None => {
                warn!("Received wl_touch::motion without seat");
                return;
            },
        };

        let touch_point = match seat_state.touch_map.get_mut(&id) {
            Some(touch_point) => touch_point,
            None => return,
        };

        let window_id = touch_point.window_id;
        let scale_factor = match self.windows.get_mut().get(&window_id) {
            Some(window) => window.lock().unwrap().scale_factor(),
            None => return,
        };

        touch_point.location = LogicalPosition::<f64>::from(position);

        self.events_sink.push_window_event(
            WindowEvent::Touch(Touch {
                device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(
                    DeviceId,
                )),
                phase: TouchPhase::Moved,
                location: touch_point.location.to_physical(scale_factor),
                force: None,
                id: id as u64,
            }),
            window_id,
        );
    }

    fn touch_cancel(&mut self, seat: u32) {
        let seat_state = match self.seats.get_mut(&seat) {
            Some(seat_state) => seat_state,
            None => {
                warn!("Received wl_touch::cancel without seat");
                return;
            },
        };

        for (id, touch_point) in seat_state.touch_map.drain() {
            let window_id = touch_point.window_id;
            let scale_factor = match self.windows.get_mut().get(&window_id) {
                Some(window) => window.lock().unwrap().scale_factor(),
                None => return,
            };

            let location = touch_point.location.to_physical(scale_factor);

            self.events_sink.push_window_event(
                WindowEvent::Touch(Touch {
                    device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(
                        DeviceId,
                    )),
                    phase: TouchPhase::Cancelled,
                    location,
                    force: None,
                    id: id as u64,
                }),
                window_id,
            );
        }
    }
}

/// The state of the touch point.
#[derive(Debug)]
pub struct TouchPoint {
    /// The window on whose surface the point is present.
    pub window_id: WindowId,

    /// The location of the point on the surface.
    pub location: LogicalPosition<f64>,
}
