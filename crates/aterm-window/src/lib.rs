// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/lib.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE. (Cut the Android, iOS,
//! Orbital and Web backends, the `rwh_04`/`rwh_05`/`serde`/`mint` features and the changelog.
//! Upstream's crate-level `deny` lint levels are `warn`: aterm's lints warn and `verify lint`
//! denies them, so a build never fails on one, and the cross-cell lint denies only bans.)
//!
//! Winit is a cross-platform window creation and event loop management library.
//!
//! # Building windows
//!
//! Before you can create a [`Window`], you first need to build an [`EventLoop`]. This is done with
//! the [`EventLoop::new()`] function.
//!
//! ```no_run
//! use aterm_window::event_loop::EventLoop;
//!
//! # // Intentionally use `fn main` for clarity
//! fn main() {
//!     let event_loop = EventLoop::new().unwrap();
//!     // ...
//! }
//! ```
//!
//! Then you create a [`Window`] with [`create_window`].
//!
//! # Event handling
//!
//! Once a [`Window`] has been created, it will generate different *events*. A [`Window`] object can
//! generate [`WindowEvent`]s when certain input events occur, such as a cursor moving over the
//! window or a key getting pressed while the window is focused. Devices can generate
//! [`DeviceEvent`]s, which contain unfiltered event data that isn't specific to a certain window.
//! Some user activity, like mouse movement, can generate both a [`WindowEvent`] *and* a
//! [`DeviceEvent`]. You can also create and handle your own custom [`Event::UserEvent`]s, if
//! desired.
//!
//! You can retrieve events by calling [`EventLoop::run_app()`]. This function will
//! dispatch events for every [`Window`] that was created with that particular [`EventLoop`], and
//! will run until [`exit()`] is used, at which point [`Event::LoopExiting`].
//!
//! Winit no longer uses a `EventLoop::poll_events() -> impl Iterator<Event>`-based event loop
//! model, since that can't be implemented properly on some platforms (e.g web, iOS) and works
//! poorly on most other platforms. However, this model can be re-implemented to an extent with
#![cfg_attr(
    any(windows_platform, macos_platform, x11_platform, wayland_platform),
    doc = "[`EventLoopExtPumpEvents::pump_app_events()`][platform::pump_events::EventLoopExtPumpEvents::pump_app_events()]"
)]
#![cfg_attr(
    not(any(windows_platform, macos_platform, x11_platform, wayland_platform)),
    doc = "`EventLoopExtPumpEvents::pump_app_events()`"
)]
//! [^1]. See that method's documentation for more reasons about why
//! it's discouraged beyond compatibility reasons.
//!
//!
//! ```no_run
//! use aterm_window::application::ApplicationHandler;
//! use aterm_window::event::WindowEvent;
//! use aterm_window::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
//! use aterm_window::window::{Window, WindowId};
//!
//! #[derive(Default)]
//! struct App {
//!     window: Option<Window>,
//! }
//!
//! impl ApplicationHandler for App {
//!     fn resumed(&mut self, event_loop: &ActiveEventLoop) {
//!         self.window = Some(event_loop.create_window(Window::default_attributes()).unwrap());
//!     }
//!
//!     fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
//!         match event {
//!             WindowEvent::CloseRequested => {
//!                 println!("The close button was pressed; stopping");
//!                 event_loop.exit();
//!             },
//!             WindowEvent::RedrawRequested => {
//!                 // Redraw the application.
//!                 //
//!                 // It's preferable for applications that do not render continuously to render in
//!                 // this event rather than in AboutToWait, since rendering in here allows
//!                 // the program to gracefully handle redraws requested by the OS.
//!
//!                 // Draw.
//!
//!                 // Queue a RedrawRequested event.
//!                 //
//!                 // You only need to call this if you've determined that you need to redraw in
//!                 // applications which do not always need to. Applications that redraw continuously
//!                 // can render here instead.
//!                 self.window.as_ref().unwrap().request_redraw();
//!             }
//!             _ => (),
//!         }
//!     }
//! }
//!
//! # // Intentionally use `fn main` for clarity
//! fn main() {
//!     let event_loop = EventLoop::new().unwrap();
//!
//!     // ControlFlow::Poll continuously runs the event loop, even if the OS hasn't
//!     // dispatched any events. This is ideal for games and similar applications.
//!     event_loop.set_control_flow(ControlFlow::Poll);
//!
//!     // ControlFlow::Wait pauses the event loop if no events are available to process.
//!     // This is ideal for non-game applications that only update in response to user
//!     // input, and uses significantly less power/CPU time than ControlFlow::Poll.
//!     event_loop.set_control_flow(ControlFlow::Wait);
//!
//!     let mut app = App::default();
//!     event_loop.run_app(&mut app);
//! }
//! ```
//!
//! [`WindowEvent`] has a [`WindowId`] member. In multi-window environments, it should be
//! compared to the value returned by [`Window::id()`] to determine which [`Window`]
//! dispatched the event.
//!
//! # Drawing on the window
//!
//! Winit doesn't directly provide any methods for drawing on a [`Window`]. However, it allows you
//! to retrieve the raw handle of the window and display (see the [`platform`] module and/or the
//! [`raw_window_handle`] and [`raw_display_handle`] methods), which in turn allows
//! you to create an OpenGL/Vulkan/DirectX/Metal/etc. context that can be used to render graphics.
//!
//! Note that many platforms will display garbage data in the window's client area if the
//! application doesn't render anything to the window by the time the desktop compositor is ready to
//! display the window to the user. If you notice this happening, you should create the window with
//! [`visible` set to `false`][crate::window::WindowAttributes::with_visible] and explicitly make
//! the window visible only once you're ready to render into it.
//!
//! # UI scaling
//!
//! UI scaling is important, go read the docs for the [`dpi`] crate for an
//! introduction.
//!
//! All of Winit's functions return physical types, but can take either logical or physical
//! coordinates as input, allowing you to use the most convenient coordinate system for your
//! particular application.
//!
//! Winit will dispatch a [`ScaleFactorChanged`] event whenever a window's scale factor has changed.
//! This can happen if the user drags their window from a standard-resolution monitor to a high-DPI
//! monitor or if the user changes their DPI settings. This allows you to rescale your application's
//! UI elements and adjust how the platform changes the window's size to reflect the new scale
//! factor. If a window hasn't received a [`ScaleFactorChanged`] event, its scale factor
//! can be found by calling [`window.scale_factor()`].
//!
//! [`ScaleFactorChanged`]: event::WindowEvent::ScaleFactorChanged
//! [`window.scale_factor()`]: window::Window::scale_factor
//!
//! # Cargo Features
//!
//! Winit provides the following Cargo features:
//!
//! * `x11` (enabled by default): On Unix platforms, enables the X11 backend.
//! * `wayland` (enabled by default): On Unix platforms, enables the Wayland backend.
//! * `rwh_06`: Implement `raw-window-handle v0.6` traits. (aterm: not a default; on macOS a
//!   window's content view is `aterm_objc::HasNsView`, and on X11 and Wayland a window's handles
//!   are `aterm_types::native_window::HasNativeWindow`.)
//!
//! See the [`platform`] module for documentation on platform-specific cargo
//! features.
//!
//! [`EventLoop`]: event_loop::EventLoop
//! [`EventLoop::new()`]: event_loop::EventLoop::new
//! [`EventLoop::run_app()`]: event_loop::EventLoop::run_app
//! [`exit()`]: event_loop::ActiveEventLoop::exit
//! [`Window`]: window::Window
//! [`WindowId`]: window::WindowId
//! [`WindowAttributes`]: window::WindowAttributes
//! [`create_window`]: event_loop::ActiveEventLoop::create_window
//! [`Window::id()`]: window::Window::id
//! [`WindowEvent`]: event::WindowEvent
//! [`DeviceEvent`]: event::DeviceEvent
//! [`Event::UserEvent`]: event::Event::UserEvent
//! [`Event::LoopExiting`]: event::Event::LoopExiting
//! [`raw_window_handle`]: ./window/struct.Window.html#method.raw_window_handle
//! [`raw_display_handle`]: ./window/struct.Window.html#method.raw_display_handle
//! [^1]: `EventLoopExtPumpEvents::pump_app_events()` is only available on Windows, macOS, X11 and Wayland.

#![warn(rust_2018_idioms)]
#![warn(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg), doc(auto_cfg(hide(doc, docsrs))))]
#![allow(clippy::missing_safety_doc)]
#![warn(clippy::uninlined_format_args)]

#[cfg(feature = "rwh_06")]
pub use rwh_06 as raw_window_handle;

// LOCAL PATCH (aterm): this crate's own module, where upstream re-exports the `dpi` crate.
pub mod dpi;

pub mod application;
#[macro_use]
pub mod error;
mod cursor;
mod cursor_icon;
pub mod event;
pub mod event_loop;
/// The shadow margin around a client-decorated Wayland window (pixels and geometry; tested on
/// every host, shown only by the Wayland backend).
#[cfg(any(test, wayland_platform))]
#[cfg_attr(not(wayland_platform), allow(dead_code))]
mod frame_shadow;
#[cfg(not(macos_platform))]
mod icon;
pub mod keyboard;
pub mod monitor;
mod platform_impl;
mod utils;
pub mod window;

pub mod platform;
