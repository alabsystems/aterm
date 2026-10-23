// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/event.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! The [`Event`] enum and supporting types for [`EventLoop::run_app`].
use std::path::PathBuf;
use std::sync::{Mutex, Weak};
use std::time::Instant;

use crate::keyboard::SmolStr;

use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::error::ExternalError;
use crate::keyboard::{self, ModifiersKeyState, ModifiersKeys, ModifiersState};
use crate::platform_impl;
#[cfg(doc)]
use crate::window::Window;
use crate::window::{Theme, WindowId};

/// Describes a generic event.
#[derive(Debug, Clone, PartialEq)]
pub enum Event<T: 'static> {
    /// See [`ApplicationHandler::new_events`](crate::application::ApplicationHandler::new_events).
    NewEvents(StartCause),

    /// See [`ApplicationHandler::window_event`](crate::application::ApplicationHandler::window_event).
    WindowEvent { window_id: WindowId, event: WindowEvent },

    /// See [`ApplicationHandler::device_event`](crate::application::ApplicationHandler::device_event).
    DeviceEvent { device_id: DeviceId, event: DeviceEvent },

    /// See [`ApplicationHandler::user_event`](crate::application::ApplicationHandler::user_event).
    UserEvent(T),

    /// See [`ApplicationHandler::suspended`](crate::application::ApplicationHandler::suspended).
    Suspended,

    /// See [`ApplicationHandler::resumed`](crate::application::ApplicationHandler::resumed).
    Resumed,

    /// See [`ApplicationHandler::about_to_wait`](crate::application::ApplicationHandler::about_to_wait).
    AboutToWait,

    /// See [`ApplicationHandler::exiting`](crate::application::ApplicationHandler::exiting).
    LoopExiting,
}

impl<T> Event<T> {
    /// Maps the user event type when `self` is not [`Event::UserEvent`].
    #[allow(clippy::result_large_err)]
    pub fn map_nonuser_event<U>(self) -> Result<Event<U>, Event<T>> {
        use self::Event::*;
        match self {
            UserEvent(_) => Err(self),
            WindowEvent { window_id, event } => Ok(WindowEvent { window_id, event }),
            DeviceEvent { device_id, event } => Ok(DeviceEvent { device_id, event }),
            NewEvents(cause) => Ok(NewEvents(cause)),
            AboutToWait => Ok(AboutToWait),
            LoopExiting => Ok(LoopExiting),
            Suspended => Ok(Suspended),
            Resumed => Ok(Resumed),
        }
    }
}

/// Describes the reason the event loop is resuming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartCause {
    /// Sent when the time specified by [`ControlFlow::WaitUntil`](crate::event_loop::ControlFlow::WaitUntil) has been reached.
    ResumeTimeReached { start: Instant, requested_resume: Instant },

    /// Sent when new OS events arrive while waiting.
    WaitCancelled { start: Instant, requested_resume: Option<Instant> },

    /// Sent when the event loop resumes under [`ControlFlow::Poll`](crate::event_loop::ControlFlow::Poll).
    Poll,

    /// Sent once immediately after `run` is called.
    Init,
}

/// Describes an event from a [`Window`].
///
/// LOCAL PATCH (aterm): no `PinchGesture`, `DoubleTapGesture`, `RotationGesture`,
/// `TouchpadPressure`, `PanGesture` or `ActivationTokenDone`: no remaining backend sends them, and
/// nothing in aterm handled one.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowEvent {
    /// The client area size of the window changed.
    Resized(PhysicalSize<u32>),

    /// The position of the window changed.
    Moved(PhysicalPosition<i32>),

    /// The window was requested to close.
    CloseRequested,

    /// The window was destroyed.
    Destroyed,

    /// A file was dropped into the window.
    DroppedFile(PathBuf),

    /// A file is being hovered over the window.
    HoveredFile(PathBuf),

    /// A hovered file exited the window.
    HoveredFileCancelled,

    /// The window gained (`true`) or lost (`false`) focus.
    Focused(bool),

    /// A keyboard event occurred.
    KeyboardInput {
        device_id: DeviceId,
        event: KeyEvent,
        /// `true` if generated synthetically on focus change.
        is_synthetic: bool,
    },

    /// The keyboard modifiers changed.
    ModifiersChanged(Modifiers),

    /// An input method event (enabled via [`Window::set_ime_allowed`]).
    Ime(Ime),

    /// The cursor moved on the window.
    CursorMoved {
        device_id: DeviceId,
        /// Pixel coordinates relative to the top-left corner of the window.
        position: PhysicalPosition<f64>,
    },

    /// The cursor entered the window.
    CursorEntered { device_id: DeviceId },

    /// The cursor left the window.
    CursorLeft { device_id: DeviceId },

    /// A mouse wheel or touchpad scroll occurred.
    MouseWheel { device_id: DeviceId, delta: MouseScrollDelta, phase: TouchPhase },

    /// A mouse button was pressed or released.
    MouseInput { device_id: DeviceId, state: ElementState, button: MouseButton },

    /// Motion on an analog axis.
    AxisMotion { device_id: DeviceId, axis: AxisId, value: f64 },

    /// A touch event occurred.
    Touch(Touch),

    /// The window's DPI scale factor changed.
    ScaleFactorChanged {
        scale_factor: f64,
        /// Handle to update the inner size during scale changes.
        inner_size_writer: InnerSizeWriter,
    },

    /// The system window theme changed.
    ThemeChanged(Theme),

    /// The window's occlusion state changed.
    Occluded(bool),

    /// The window should be redrawn (invalidated by the OS or requested via [`Window::request_redraw`]).
    RedrawRequested,
}

/// Identifier of an input device.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(pub(crate) platform_impl::DeviceId);

impl DeviceId {
    /// Returns a dummy id for unit testing.
    pub const fn dummy() -> Self {
        DeviceId(platform_impl::DeviceId::dummy())
    }
}

/// Raw hardware events not associated with any particular window.
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceEvent {
    Added,
    Removed,

    /// Raw, unfiltered physical motion of a pointing device.
    MouseMotion {
        /// `(x, y)` change in position in device-specific units.
        delta: (f64, f64),
    },

    /// Physical scroll event.
    MouseWheel {
        delta: MouseScrollDelta,
    },

    /// Motion on an analog axis.
    Motion {
        axis: AxisId,
        value: f64,
    },

    Button {
        button: ButtonId,
        state: ElementState,
    },

    Key(RawKeyEvent),
}

/// A keyboard input as a raw device event.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct RawKeyEvent {
    /// Physical key identifier.
    pub physical_key: keyboard::PhysicalKey,
    /// Press or release state.
    pub state: ElementState,
}

/// A keyboard input targeting a window.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEvent {
    /// Position of the key independent of the active layout.
    pub physical_key: keyboard::PhysicalKey,

    #[cfg_attr(
        not(any(windows_platform, macos_platform, x11_platform, wayland_platform)),
        allow(rustdoc::broken_intra_doc_links)
    )]
    /// Logical key value affected by all modifiers except Ctrl (see [`key_without_modifiers`]).
    pub logical_key: keyboard::Key,

    /// Text produced by this keypress, or `None` if non-printable.
    pub text: Option<SmolStr>,

    /// Physical location of this key on the keyboard (see [`KeyLocation`](crate::keyboard::KeyLocation)).
    pub location: keyboard::KeyLocation,

    /// Whether the key is being pressed or released.
    pub state: ElementState,

    /// `true` if this event is a key repeat.
    ///
    /// ```
    /// use aterm_window::event::{ElementState, KeyEvent, WindowEvent};
    /// use aterm_window::keyboard::{KeyCode, PhysicalKey};
    /// # let window_event = WindowEvent::RedrawRequested; // To make the example compile
    /// match window_event {
    ///     WindowEvent::KeyboardInput {
    ///         event:
    ///             KeyEvent {
    ///                 physical_key: PhysicalKey::Code(KeyCode::KeyW),
    ///                 state: ElementState::Pressed,
    ///                 repeat: false,
    ///                 ..
    ///             },
    ///         ..
    ///     } => {
    ///         // The physical key `W` was pressed, and it was not a repeat
    ///     },
    ///     _ => {}, // Handle other events
    /// }
    /// ```
    pub repeat: bool,

    /// Platform-specific key event fields.
    pub(crate) platform_specific: platform_impl::KeyEventExtra,
}

#[cfg(any(test, debug_assertions))]
impl KeyEvent {
    /// Constructs a synthetic key event for conformance tests.
    #[doc(hidden)]
    #[must_use]
    pub fn synthetic_for_test(
        physical_key: keyboard::PhysicalKey,
        logical_key: keyboard::Key,
        text: Option<SmolStr>,
        location: keyboard::KeyLocation,
        state: ElementState,
        repeat: bool,
    ) -> Self {
        let key_without_modifiers = logical_key.clone();
        Self::synthetic_for_test_with_base(
            physical_key,
            logical_key,
            key_without_modifiers,
            text,
            location,
            state,
            repeat,
        )
    }

    /// Like [`synthetic_for_test`](Self::synthetic_for_test), with an explicit `key_without_modifiers` base key.
    #[doc(hidden)]
    #[must_use]
    pub fn synthetic_for_test_with_base(
        physical_key: keyboard::PhysicalKey,
        logical_key: keyboard::Key,
        key_without_modifiers: keyboard::Key,
        text: Option<SmolStr>,
        location: keyboard::KeyLocation,
        state: ElementState,
        repeat: bool,
    ) -> Self {
        let platform_specific = platform_impl::KeyEventExtra {
            text_with_all_modifiers: text.clone(),
            key_without_modifiers,
            #[cfg(any(x11_platform, wayland_platform))]
            text_ignoring_lock: text.clone(),
        };

        Self { physical_key, logical_key, text, location, state, repeat, platform_specific }
    }

    /// Sets [`text_ignoring_lock`] on X11 and Wayland; a no-op elsewhere.
    #[doc(hidden)]
    #[must_use]
    pub fn with_text_ignoring_lock_for_test(self, text: Option<SmolStr>) -> Self {
        #[cfg(any(x11_platform, wayland_platform))]
        {
            let mut this = self;
            this.platform_specific.text_ignoring_lock = text;
            this
        }
        #[cfg(not(any(x11_platform, wayland_platform)))]
        {
            let _ = text;
            self
        }
    }
}

/// Describes a keyboard modifiers event.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Modifiers {
    pub(crate) state: ModifiersState,
    pub(crate) pressed_mods: ModifiersKeys,
}

impl Modifiers {
    /// The state of the modifiers.
    pub const fn state(&self) -> ModifiersState {
        self.state
    }

    /// The state of the left shift key.
    pub const fn lshift_state(&self) -> ModifiersKeyState {
        self.mod_state(ModifiersKeys::LSHIFT)
    }

    /// The state of the right shift key.
    pub const fn rshift_state(&self) -> ModifiersKeyState {
        self.mod_state(ModifiersKeys::RSHIFT)
    }

    /// The state of the left alt key.
    pub const fn lalt_state(&self) -> ModifiersKeyState {
        self.mod_state(ModifiersKeys::LALT)
    }

    /// The state of the right alt key.
    pub const fn ralt_state(&self) -> ModifiersKeyState {
        self.mod_state(ModifiersKeys::RALT)
    }

    /// The state of the left control key.
    pub const fn lcontrol_state(&self) -> ModifiersKeyState {
        self.mod_state(ModifiersKeys::LCONTROL)
    }

    /// The state of the right control key.
    pub const fn rcontrol_state(&self) -> ModifiersKeyState {
        self.mod_state(ModifiersKeys::RCONTROL)
    }

    /// The state of the left super key.
    pub const fn lsuper_state(&self) -> ModifiersKeyState {
        self.mod_state(ModifiersKeys::LSUPER)
    }

    /// The state of the right super key.
    pub const fn rsuper_state(&self) -> ModifiersKeyState {
        self.mod_state(ModifiersKeys::RSUPER)
    }

    const fn mod_state(&self, modifier: ModifiersKeys) -> ModifiersKeyState {
        if self.pressed_mods.contains(modifier) {
            ModifiersKeyState::Pressed
        } else {
            ModifiersKeyState::Unknown
        }
    }
}

impl From<ModifiersState> for Modifiers {
    fn from(value: ModifiersState) -> Self {
        Self { state: value, pressed_mods: Default::default() }
    }
}

/// Input method (composition) events; see [`Window::set_ime_cursor_area`].
///
/// ```
/// use aterm_window::event::Ime;
///
/// let events = [
///     // Press "`" key.
///     Ime::Preedit("`".into(), Some((0, 0))),
///     // Press "E" key; clear preedit before committing.
///     Ime::Preedit(String::new(), None),
///     Ime::Commit("é".into()),
/// ];
/// assert_eq!(events.last(), Some(&Ime::Commit("é".into())));
/// ```
///
/// ```
/// use aterm_window::event::Ime;
///
/// let events = [
///     // Press "A" key.
///     Ime::Preedit("a".into(), Some((1, 1))),
///     // Press "B" key.
///     Ime::Preedit("a b".into(), Some((3, 3))),
///     // Press left arrow key.
///     Ime::Preedit("a b".into(), Some((1, 1))),
///     // Press space key.
///     Ime::Preedit("啊b".into(), Some((3, 3))),
///     // Press space key; clear preedit before committing.
///     Ime::Preedit(String::new(), None),
///     Ime::Commit("啊不".into()),
/// ];
/// assert_eq!(events.last(), Some(&Ime::Commit("啊不".into())));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ime {
    /// The IME was enabled.
    Enabled,

    /// Composing preedit string and optional byte-indexed `(start, end)` cursor range.
    Preedit(String, Option<(usize, usize)>),

    /// Committed text to insert (preceded by an empty [`Self::Preedit`]).
    Commit(String),

    /// The IME was disabled.
    Disabled,
}

/// Describes touch-screen input state.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

/// Represents a touch event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    /// Input device identifier.
    pub device_id: DeviceId,
    /// Touch phase.
    pub phase: TouchPhase,
    /// Touch position in the window.
    pub location: PhysicalPosition<f64>,
    /// Touch pressure, if supported by the platform.
    pub force: Option<Force>,
    /// Unique identifier of a finger.
    pub id: u64,
}

/// Describes the force of a touch event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Force {
    /// Calibrated force where `1.0` represents an average touch.
    Calibrated {
        /// Force of the touch along the stylus axis.
        force: f64,
        /// Maximum possible force for a touch.
        max_possible_force: f64,
        /// Altitude angle (in radians) of the stylus, if available.
        altitude_angle: Option<f64>,
    },
    /// Force normalized to `0.0..=1.0` by the platform.
    Normalized(f64),
}

impl Force {
    /// Returns the force normalized to `0.0..=1.0`.
    #[inline]
    pub fn normalized(&self) -> f64 {
        match self {
            Force::Calibrated { force, max_possible_force, altitude_angle } => {
                let force = match altitude_angle {
                    Some(altitude_angle) => force / altitude_angle.sin(),
                    None => *force,
                };
                force / max_possible_force
            },
            Force::Normalized(force) => *force,
        }
    }
}

/// Identifier for a specific analog axis on some device.
pub type AxisId = u32;

/// Identifier for a specific button on some device.
pub type ButtonId = u32;

/// Describes the input state of a key.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum ElementState {
    Pressed,
    Released,
}

impl ElementState {
    /// True if `self == Pressed`.
    #[inline]
    pub const fn is_pressed(self) -> bool {
        matches!(self, ElementState::Pressed)
    }
}

/// Describes a button of a mouse controller.
#[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(u16),
}

/// Describes a difference in the mouse scroll wheel state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseScrollDelta {
    /// Horizontal and vertical scroll amount in lines or rows.
    LineDelta(f32, f32),

    /// Horizontal and vertical scroll amount in pixels.
    PixelDelta(PhysicalPosition<f64>),
}

/// Handle to synchronously change the size of the window from [`WindowEvent::ScaleFactorChanged`].
#[derive(Debug, Clone)]
pub struct InnerSizeWriter {
    pub(crate) new_inner_size: Weak<Mutex<PhysicalSize<u32>>>,
}

impl InnerSizeWriter {
    #[inline]
    pub(crate) const fn new(new_inner_size: Weak<Mutex<PhysicalSize<u32>>>) -> Self {
        Self { new_inner_size }
    }

    /// Requests a new inner size to be set synchronously on the window.
    #[inline]
    pub fn request_inner_size(
        &mut self,
        new_inner_size: PhysicalSize<u32>,
    ) -> Result<(), ExternalError> {
        if let Some(inner) = self.new_inner_size.upgrade() {
            *inner.lock().unwrap() = new_inner_size;
            Ok(())
        } else {
            Err(ExternalError::Ignored)
        }
    }
}

impl PartialEq for InnerSizeWriter {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.new_inner_size.as_ptr() == other.new_inner_size.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use crate::event;

    macro_rules! foreach_event {
        ($closure:expr) => {{
            #[allow(unused_mut)]
            let mut x = $closure;
            let did = event::DeviceId::dummy();

            #[allow(deprecated)]
            {
                use crate::event::Event::*;
                use crate::event::Ime::Enabled;
                use crate::event::WindowEvent::*;
                use crate::window::WindowId;

                let wid = WindowId::dummy();
                x(UserEvent(()));
                x(NewEvents(event::StartCause::Init));
                x(AboutToWait);
                x(LoopExiting);
                x(Suspended);
                x(Resumed);

                let with_window_event = |wev| x(WindowEvent { window_id: wid, event: wev });

                with_window_event(CloseRequested);
                with_window_event(Destroyed);
                with_window_event(Focused(true));
                with_window_event(Moved((17, -9).into()));
                with_window_event(Resized((321, 123).into()));
                with_window_event(DroppedFile("x.txt".into()));
                with_window_event(HoveredFile("x.txt".into()));
                with_window_event(HoveredFileCancelled);
                with_window_event(Ime(Enabled));
                with_window_event(CursorMoved { device_id: did, position: (0, 0).into() });
                with_window_event(ModifiersChanged(event::Modifiers::default()));
                with_window_event(CursorEntered { device_id: did });
                with_window_event(CursorLeft { device_id: did });
                with_window_event(MouseWheel {
                    device_id: did,
                    delta: event::MouseScrollDelta::LineDelta(0.0, 0.0),
                    phase: event::TouchPhase::Started,
                });
                with_window_event(MouseInput {
                    device_id: did,
                    state: event::ElementState::Pressed,
                    button: event::MouseButton::Other(0),
                });
                with_window_event(AxisMotion { device_id: did, axis: 0, value: 0.0 });
                with_window_event(Touch(event::Touch {
                    device_id: did,
                    phase: event::TouchPhase::Started,
                    location: (0.0, 0.0).into(),
                    id: 0,
                    force: Some(event::Force::Normalized(0.0)),
                }));
                with_window_event(ThemeChanged(crate::window::Theme::Light));
                with_window_event(Occluded(true));
            }

            #[allow(deprecated)]
            {
                use event::DeviceEvent::*;

                let with_device_event =
                    |dev_ev| x(event::Event::DeviceEvent { device_id: did, event: dev_ev });

                with_device_event(Added);
                with_device_event(Removed);
                with_device_event(MouseMotion { delta: (0.0, 0.0).into() });
                with_device_event(MouseWheel {
                    delta: event::MouseScrollDelta::LineDelta(0.0, 0.0),
                });
                with_device_event(Motion { axis: 0, value: 0.0 });
                with_device_event(Button { button: 0, state: event::ElementState::Pressed });
            }
        }};
    }

    #[test]
    fn test_map_nonuser_event() {
        foreach_event!(|event: event::Event<()>| {
            let is_user = matches!(event, event::Event::UserEvent(()));
            let expected = event.clone();
            let event2 = event.map_nonuser_event::<()>();
            if is_user {
                assert_eq!(event2, Err(event::Event::UserEvent(())));
            } else {
                assert_eq!(event2, Ok(expected), "variant and payload survive mapping");
            }
        })
    }

    #[test]
    fn test_force_normalize() {
        let force = event::Force::Normalized(0.0);
        assert_eq!(force.normalized(), 0.0);

        let force2 =
            event::Force::Calibrated { force: 5.0, max_possible_force: 2.5, altitude_angle: None };
        assert_eq!(force2.normalized(), 2.0);

        let force3 = event::Force::Calibrated {
            force: 5.0,
            max_possible_force: 2.5,
            altitude_angle: Some(std::f64::consts::PI / 2.0),
        };
        assert_eq!(force3.normalized(), 2.0);
    }
}
