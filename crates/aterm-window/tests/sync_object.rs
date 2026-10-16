// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 tests/sync_object.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

#[allow(dead_code)]
fn needs_sync<T: Sync>() {}

#[test]
fn event_loop_proxy_send() {
    #[allow(dead_code)]
    fn is_send<T: 'static + Send>() {
        // ensures that `winit::EventLoopProxy<T: Send>` implements `Sync`
        needs_sync::<winit::event_loop::EventLoopProxy<T>>();
    }
}

#[test]
fn window_sync() {
    // ensures that `winit::Window` implements `Sync`
    needs_sync::<winit::window::Window>();
}

#[test]
fn window_builder_sync() {
    needs_sync::<winit::window::WindowAttributes>();
}

// Custom cursors left macOS in winit-core step 5 (2026-10-16).
#[cfg(not(target_os = "macos"))]
#[test]
fn custom_cursor_sync() {
    needs_sync::<winit::window::CustomCursorSource>();
    needs_sync::<winit::window::CustomCursor>();
}
