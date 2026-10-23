// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/lib.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Cross-platform window creation and event loop management library.
//!
//! You can retrieve events by calling `EventLoop::run_app()`:
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
//!                 // applications which do not always need to. Applications that redraw
//!                 // continuously can render here instead.
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
#![cfg_attr(
    any(windows_platform, macos_platform, x11_platform, wayland_platform),
    doc = "[`EventLoopExtPumpEvents::pump_app_events()`][platform::pump_events::EventLoopExtPumpEvents::pump_app_events()]"
)]
#![cfg_attr(
    not(any(windows_platform, macos_platform, x11_platform, wayland_platform)),
    doc = "`EventLoopExtPumpEvents::pump_app_events()`"
)]
//! is only available on Windows, macOS, X11 and Wayland.

#![warn(rust_2018_idioms)]
#![warn(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg), doc(auto_cfg(hide(doc, docsrs))))]
#![allow(clippy::missing_safety_doc)]
#![warn(clippy::uninlined_format_args)]

#[cfg(feature = "rwh_06")]
pub use rwh_06 as raw_window_handle;

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
