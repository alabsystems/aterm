// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ORACLE: every `tracing` invocation form in aterm's dependency graph,
//! copied verbatim from the consumer that writes it.
//!
//! # Why this file exists
//!
//! Three of the four crates this shim serves cannot be compiled on the machine
//! that develops it. `zbus 5.16.0` and `tiny-xlib 0.2.5` are Linux-only edges
//! (via `accesskit_unix` and `softbuffer` respectively), and the winit fork's
//! X11 / Wayland / Windows / web / Android / iOS backends are all `cfg`-ed out
//! on macOS. "The build is green" therefore proves almost nothing about this
//! crate's surface: it exercises the macOS winit backend and little else.
//!
//! So the proof is moved into a place that *does* compile everywhere. Every
//! invocation below is a byte-for-byte copy of a real call site, carrying a
//! comment naming the crate, file and line it came from. Only the surrounding
//! scaffolding — the locals, structs and methods the arguments mention — is
//! written here. **Compiling this test is the proof that the surface accepts
//! what the real consumers write.** When a consumer bumps and introduces a form
//! this shim cannot parse, the fix is to paste the new line in here first and
//! watch it fail.
//!
//! A handful of invocations are marked `(synthetic)`. Those cover a name that a
//! verbatim `use` line imports but whose own call site lives in a different
//! file of the same crate; they exist so `#![deny(unused_imports)]` below has
//! something to bite on for every imported name.
//!
//! # Two invariants this file holds, both mechanically checkable
//!
//! 1. **Every distinct `use tracing::…` line in the four consumer trees appears
//!    here verbatim.** There are fifteen of them. Re-derive the list with
//!    `grep -rh "use tracing::" vendor/winit/src <registry>/{softbuffer-0.4.8,
//!    zbus-5.16.0,tiny-xlib-0.2.5}/src | sort -u` and check each against this
//!    file. A module whose import line is *narrowed* to only the names it
//!    happens to call is not a copy of the consumer any more, and it quietly
//!    weakens the `deny(unused_imports)` assertion below.
//! 2. **Every distinct argument SHAPE appears here.** Shape means the token
//!    classes, not the identifiers: `warn!(MSG, IDENT)` and
//!    `debug!(MSG, IDENT)` are one shape, and `trace!(key = _i, MSG)` is a
//!    different one from `trace!(MSG, key)`. There are 51 across the four
//!    crates; the surplus in this file is span and sigil combinations that no
//!    consumer writes yet, kept because they cost a line.
//!
//! Neither invariant is enforced by the compiler. They are what a reviewer
//! re-derives, and they are the reason this file is worth more than the sum of
//! its `assert!`s.
//!
//! # The forms that are NOT a bare `mac!(…);`
//!
//! It is easy to read a list of macro calls and conclude the grammar is the
//! whole test. It is not. Three shapes here are about the *statement* rather
//! than the arguments, and each was absent from an earlier revision of this
//! file even though every one of its arguments was covered:
//!
//! * **An attributed macro statement** — all five tiny-xlib call sites are
//!   `#[cfg(…)] tracing::error!(…);`, so what rustc has to accept is
//!   `#[attr] {};`, and one of them sits under an attributed *block*.
//! * **`#[instrument]` on a non-`async fn`** — two of zbus's twenty-three, and
//!   upstream `tracing-attributes` generates a materially different expansion
//!   for the two cases, so the async ones are not evidence about these.
//! * **`#[instrument]` stacked above another attribute** — zbus's
//!   `handshake/client.rs:61` puts `#[cfg]` *underneath* it, which means the
//!   pass-through has to hand the inner attribute back for rustc to evaluate
//!   after expansion.
//!
//! # The runtime half
//!
//! Accepting the syntax is only half the contract. The other half is that a
//! disabled callsite evaluates **nothing**, which is what makes the shim
//! behaviourally identical to upstream rather than merely quiet. That is pinned
//! by `macros_never_evaluate_their_arguments`, which arms two tripwires
//! (a counter and a panic), proves both are live, and then fires every macro at
//! them.
//!
//! # What the lint attributes below are evidence of
//!
//! `#![deny(unused_imports)]` is a load-bearing assertion, not hygiene: it
//! proves the claim in the shim's module docs that `use tracing::warn;` is
//! still *used* even though `warn!` expands to nothing. Name resolution runs
//! before expansion, so the import is what resolves the macro. If that ever
//! changed, this file would stop compiling — which is the point.
//!
//! The three `allow`s are the mirror image: they are the documented divergence
//! from upstream, demonstrated. Discarding the token trees leaves the values
//! they mention genuinely unmentioned, so
//!
//! * `unused_variables` — a local read only inside a macro argument, and its
//!   nastier sibling, a *closure pattern binding* read only inside one
//!   (tiny-xlib's `|(_i, handler)|` at `lib.rs:268`, where there is nowhere to
//!   move the binding to),
//! * `dead_code` — a method called only inside a macro argument,
//! * `unused_unsafe` — an `unsafe {}` block whose only unsafe operation was
//!   inside a macro argument (winit's `x11/mod.rs:201`),
//!
//! all fire here. Every real file affected is third-party or vendored and none
//! is built with `-D warnings`; no first-party aterm crate uses `tracing` at
//! all. The alternative, keeping the names "used" with `let _ = &$value;`,
//! would evaluate the arguments — see the shim's module docs for why that is
//! the one thing this crate must not do.

#![deny(unused_imports)]
#![allow(unused_variables, dead_code, unused_unsafe)]

use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

// The crate-root macro imports the tripwire test fires. Every other module
// below carries its own `use` line, copied verbatim from the consumer it
// stands for.
use tracing::{debug, debug_span, error, info, info_span, trace, trace_span, warn};

// ===========================================================================
// Tripwires — the runtime half of the contract
// ===========================================================================

/// Bumped once per real evaluation of [`tripwire`].
static EVALUATIONS: AtomicUsize = AtomicUsize::new(0);

/// An identity function with an observable side effect.
///
/// Placed in macro argument positions. If the shim ever evaluated its
/// arguments, the counter would move.
fn tripwire<T>(value: T) -> T {
    EVALUATIONS.fetch_add(1, Ordering::SeqCst);
    value
}

/// An argument that destroys the test if it is ever evaluated.
///
/// The counter alone would catch an accidental evaluation; this catches it
/// louder, and covers the `let _ = &$value;` shape specifically — taking a
/// reference to `detonate()` still calls it.
fn detonate() -> &'static str {
    panic!("a tracing macro evaluated its argument");
}

// ===========================================================================
// A one-file executor, so the `Instrument` half can be *run*, not just typed
// ===========================================================================

/// Poll a future to completion on this thread.
///
/// Hand-rolled rather than pulled from `futures`: this crate has no
/// dependencies and a test that added one would undercut the point. Every
/// future here is ready on the first poll, so the loop is a formality.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

/// Stands in for `async_executor::Executor::spawn`, which is where zbus sends
/// every `.instrument(…)` result.
///
/// The bounds are the real ones. If `Instrumented<F>` ever stopped being
/// `Future + Send + 'static` for a `Future + Send + 'static` inner future,
/// zbus would stop compiling on Linux — and this line stops compiling here
/// first, on every platform.
fn executor_spawn<F: Future + Send + 'static>(future: F) -> F {
    future
}

// ===========================================================================
// winit (fork) — vendor/winit
// ===========================================================================

// THE `rustfmt::skip` ON EVERY MODULE BELOW IS LOAD-BEARING.
//
// The invocations in these modules are byte-for-byte copies of third-party
// source, and their exact shape — where the line breaks fall, which trailing
// commas are present, how a string continuation is indented — is part of what
// is being tested. rustfmt would happily rewrite
// `warn!(other_native_id, "…")` into a three-line form that winit does not
// contain, at which point the file would still compile and would no longer be
// evidence of anything. So the consumer modules are frozen and only the
// scaffolding at the top and the tests at the bottom are formatted normally.

#[rustfmt::skip]
mod winit_android_mod {
    // vendor/winit/src/platform_impl/android/mod.rs:13
    use tracing::{debug, trace, warn};

    pub fn forms() {
        let device_id = 0u32;
        let phase = 1u8;
        let location = (0.0f64, 0.0f64);
        let pointer = 2u8;

        // vendor/winit/src/platform_impl/android/mod.rs:407
        trace!(
            "Input event {device_id:?}, {phase:?}, loc={location:?}, \
             pointer={pointer:?}"
        );

        // (synthetic) `debug` and `warn` ride the same `use` line; their
        // verbatim sites are in other winit files, reproduced further down.
        debug!("android: debug");
        warn!("android: warn");
    }
}

#[rustfmt::skip]
mod winit_wayland {
    // vendor/winit/src/platform_impl/linux/wayland/window/state.rs:8
    use tracing::{info, warn};

    // `single_match` is right in general and wrong here: the `match` is the
    // point. Both winit sites put the macro in EXPRESSION position as a match
    // arm with no trailing semicolon, which is the form that would break if the
    // shim expanded to nothing at all instead of to `{}`. Collapsing them to
    // `if let` would delete the thing under test.
    #[allow(clippy::single_match)]
    pub fn forms() {
        let blur_manager: Option<()> = None;
        match blur_manager {
            Some(()) => {},
            // vendor/winit/src/platform_impl/linux/wayland/window/state.rs:1089
            // Expression position, no trailing semicolon: the macro IS the arm.
            None => info!("Blur manager unavailable, unable to change blur"),
        }

        let value = 0x2u32;
        match value {
            0 => {},
            // vendor/winit/src/platform_impl/linux/wayland/seat/keyboard/mod.rs:61
            _ => warn!("unknown keymap format 0x{:x}", value),
        }
    }
}

#[rustfmt::skip]
mod winit_macos_util {
    // vendor/winit/src/platform_impl/macos/util.rs:1
    use tracing::trace;

    pub struct TraceGuard {
        pub module_path: &'static str,
        pub called_from_fn: &'static str,
    }

    impl TraceGuard {
        pub fn new(module_path: &'static str, called_from_fn: &'static str) -> Self {
            // vendor/winit/src/platform_impl/macos/util.rs:17
            // `target` here is an ORDINARY FIELD NAME reached with `=`, not the
            // `target:` colon prefix (which no consumer uses).
            trace!(target = module_path, "Triggered `{}`", called_from_fn);
            Self { module_path, called_from_fn }
        }
    }

    impl Drop for TraceGuard {
        fn drop(&mut self) {
            // vendor/winit/src/platform_impl/macos/util.rs:25
            trace!(target = self.module_path, "Completed `{}`", self.called_from_fn);
        }
    }

    pub fn forms() {
        let _guard = TraceGuard::new("winit::platform_impl::macos", "orderOut:");
    }
}

/// The span pattern from winit's public API, always fully qualified
/// (`tracing::debug_span!`) and always immediately `.entered()`.
///
/// MEASURED, not estimated: `vendor/winit/src` contains exactly 68
/// `debug_span!` invocations and exactly 68 `.entered()` calls, and every one
/// of the 68 spans is `.entered()` on the spot. No winit span is bound to a
/// local, passed to `.instrument(…)`, `.record()`-ed or `.exit()`-ed. The
/// distinct argument shapes among those 68 are the nine reproduced below.
#[rustfmt::skip]
mod winit_spans {
    pub fn forms() {
        // vendor/winit/src/event_loop.rs:116
        let _span = tracing::debug_span!("winit::EventLoopBuilder::build").entered();

        // vendor/winit/src/window.rs:496 — name only, WITH a trailing comma.
        let _span = tracing::debug_span!("winit::Window::id",).entered();

        let title = "aterm";
        // vendor/winit/src/window.rs:925
        let _span = tracing::debug_span!("winit::Window::set_title", title).entered();

        let width = 32u32;
        let height = 32u32;
        // vendor/winit/src/icon.rs:113
        let _span = tracing::debug_span!("winit::Icon::from_rgba", width, height).entered();

        let hotspot_x = 0u16;
        let hotspot_y = 0u16;
        // vendor/winit/src/cursor.rs:92
        let _span =
            tracing::debug_span!("winit::Cursor::from_rgba", width, height, hotspot_x, hotspot_y)
                .entered();

        let position = (0i32, 0i32);
        // vendor/winit/src/window.rs:736
        let _span = tracing::debug_span!(
            "winit::Window::set_outer_position",
            position = ?position
        )
        .entered();

        let size = (0u32, 0u32);
        // vendor/winit/src/window.rs:1251
        let _span = tracing::debug_span!(
            "winit::Window::set_ime_cursor_area",
            position = ?position,
            size = ?size,
        )
        .entered();

        let allowed = true;
        // vendor/winit/src/event_loop.rs:287
        let _span = tracing::debug_span!(
            "winit::EventLoop::listen_device_events",
            allowed = ?allowed
        )
        .entered();

        let protected = false;
        // vendor/winit/src/window.rs:1399
        let _span =
            tracing::debug_span!("winit::Window::set_content_protected", protected).entered();
    }
}

/// `use tracing::error;` on its own — winit's only lone-`error` import, and the
/// only place in the graph where a tracing macro is the `else` arm of an
/// `if let` inside a `block2::RcBlock` closure.
#[rustfmt::skip]
mod winit_macos_observer {
    // vendor/winit/src/platform_impl/macos/observer.rs:23
    use tracing::error;

    pub fn forms() {
        let mut closure: Option<Box<dyn FnOnce()>> = Some(Box::new(|| {}));
        // The `RcBlock` shape, minus the Objective-C: the macro is the tail
        // expression of the `else` arm, with no trailing semicolon.
        if let Some(closure) = closure.take() {
            closure()
        } else {
            // vendor/winit/src/platform_impl/macos/observer.rs:179
            error!("tried to execute queued closure on main thread twice");
        }
    }
}

#[rustfmt::skip]
mod winit_macos {
    // vendor/winit/src/platform_impl/macos/window_delegate.rs:30
    use tracing::{trace, warn};

    #[derive(Debug)]
    pub struct Monitor(pub u32);

    impl Monitor {
        pub fn name(&self) -> Option<String> {
            Some(format!("Display {}", self.0))
        }

        pub fn video_modes(&self) {
            // vendor/winit/src/platform_impl/macos/monitor.rs:298
            warn!(monitor = ?self, "failed to get a list of display modes");
        }
    }

    /// Objective-C `NSAppearance` stand-in: `name()` is `unsafe`, exactly as in
    /// winit, because that is what makes `%unsafe { … }` a *behavioural* test
    /// and not just a syntactic one. Evaluating this argument would run an
    /// unsafe call upstream leaves untouched on a disabled callsite.
    pub struct Appearance;

    impl Appearance {
        pub unsafe fn name(&self) -> &'static str {
            "NSAppearanceNameAqua"
        }
    }

    pub fn forms() {
        let other_native_id = 7u32;
        // vendor/winit/src/platform_impl/macos/monitor.rs:361
        warn!(other_native_id, "comparing against screen with invalid display ID");

        let display_id = 3u32;
        // vendor/winit/src/platform_impl/macos/window_delegate.rs:1621
        warn!(display_id, "got screen with invalid display ID");

        let appearance = Appearance;
        // vendor/winit/src/platform_impl/macos/window_delegate.rs:1907
        warn!(?appearance, "failed to determine the theme of the appearance");

        let theme = "dark";
        // vendor/winit/src/platform_impl/macos/window_delegate.rs:1921
        warn!(?theme, "could not find appearance for theme");

        let event = "RedrawRequested";
        // vendor/winit/src/platform_impl/macos/app_state.rs:354
        tracing::debug!(?event, "had to queue event since another is currently being handled");

        let old = Appearance;
        let new = Appearance;
        // vendor/winit/src/platform_impl/macos/window_delegate.rs:474
        trace!(old = %unsafe { old.name() }, new = %unsafe { new.name() }, "effectiveAppearance changed");

        let monitor = Monitor(1);
        // vendor/winit/src/platform_impl/macos/window_delegate.rs:1735
        warn!(
            monitor = monitor.name(),
            "Tried to restore exclusive fullscreen on a monitor that is no longer available"
        );
        monitor.video_modes();

        let sel = "IBeamCursor";
        // vendor/winit/src/platform_impl/macos/cursor.rs:73
        tracing::warn!("cursor `{sel}` appears to be invalid");

        let translate_result = -50i32;
        // vendor/winit/src/platform_impl/macos/event.rs:66
        tracing::error!("`UCKeyTranslate` returned with the non-zero value: {}", translate_result);
    }
}

#[rustfmt::skip]
mod winit_x11 {
    // NOT covered by the file-level `deny(unused_imports)`, and the exception
    // is itself a finding. `CStr` is an ORDINARY import whose only mention in
    // this module is inside a macro argument, so after expansion it really is
    // unused and rustc is right to say so. That is a second-order case of the
    // shim's one divergence, and it is strictly narrower than it looks: winit's
    // real `x11/mod.rs` also calls `CStr::from_ptr` at line 998, outside any
    // macro, so the import stays used there. The `use tracing::…` line below
    // needs no such escape hatch, which is the whole point of the file-level
    // deny.
    #[allow(unused_imports)]
    use std::ffi::CStr;

    use tracing::{debug, info, warn};

    pub struct ImeClientData {
        pub text: String,
    }

    pub struct ImeCallData {
        pub chg_first: i32,
        pub chg_length: i32,
    }

    pub fn forms() {
        let scale_factor = 1.0f64;
        // vendor/winit/src/platform_impl/linux/x11/window.rs:192
        info!("Guessed window scale factor: {}", scale_factor);

        let dimensions = (800u32, 600u32);
        // vendor/winit/src/platform_impl/linux/x11/window.rs:219
        debug!("Calculated physical dimensions: {}x{}", dimensions.0, dimensions.1);

        let state = 0i32;
        // vendor/winit/src/platform_impl/linux/x11/mod.rs:213 — inline capture
        // with a format spec (`{state:#?}`).
        warn!("Failed to open input method: {state:#?}");

        // The two `CStr::from_ptr` calls below are `unsafe`, and the enclosing
        // `unsafe {}` block therefore becomes empty after expansion — that is
        // the `unused_unsafe` allow at the top of the file, earning its keep.
        let unsupported_locale = c"en_US.UTF-8".as_ptr();
        let default_locale = c"C".as_ptr();
        unsafe {
            // vendor/winit/src/platform_impl/linux/x11/mod.rs:201
            warn!(
                "Unsupported locale \"{}\". Restoring default locale \"{}\".",
                CStr::from_ptr(unsupported_locale).to_string_lossy(),
                CStr::from_ptr(default_locale).to_string_lossy()
            );
        }

        // vendor/winit/src/platform_impl/linux/x11/util/randr.rs:107 — a
        // string-continuation literal as the sole argument, expression position.
        warn!(
            "The WINIT_HIDPI_FACTOR environment variable is deprecated; use \
             WINIT_X11_SCALE_FACTOR"
        )
    }

    pub fn ime_forms(client_data: &ImeClientData, call_data: &ImeCallData) {
        // vendor/winit/src/platform_impl/linux/x11/ime/context.rs:83
        tracing::warn!(
            "invalid chg range: buffer length={}, but chg_first={} chg_length={}",
            client_data.text.len(),
            call_data.chg_first,
            call_data.chg_length
        );

        let error = "BadWindow";
        // vendor/winit/src/platform_impl/linux/mod.rs:690
        tracing::error!("X11 error: {:#?}", error);

        let bytes = [0xffu8, 0xfe];
        let e = "invalid utf-8";
        // vendor/winit/src/platform_impl/linux/common/xkb/mod.rs:413
        tracing::warn!("UTF-8 received from libxkbcommon ({:?}) was invalid: {e}", bytes)
    }
}

#[rustfmt::skip]
mod winit_windows {
    use tracing::{debug, trace, warn};

    #[allow(non_snake_case)]
    fn GetLastError() -> u32 {
        0
    }

    pub fn forms() {
        let hr = 0x8000_4005u32;
        // vendor/winit/src/platform_impl/windows/window.rs:1244
        warn!("Setting transparent window is failed. HRESULT Code: 0x{:X}", hr);

        // vendor/winit/src/platform_impl/windows/event_loop.rs:765 — note the
        // TRAILING COMMA after the last positional argument.
        tracing::warn!("Failed to MsgWaitForMultipleObjectsEx: error code {}", GetLastError(),);

        // vendor/winit/src/platform_impl/windows/keyboard.rs:171
        trace!(
            "Received a CHAR message but no `event_info` was available. The \
             message is probably IME, returning."
        );

        // vendor/winit/src/platform_impl/windows/drop_handler.rs:204
        debug!("Error occurred while processing dropped/hovered item: item is not a file.");

        let res: Result<(), u32> = Err(0x57);
        if let Err(error_code) = res {
            // vendor/winit/src/platform_impl/windows/event_loop.rs:736
            tracing::trace!("Failed to set high resolution timer: last error {}", error_code);
        }
    }
}

#[rustfmt::skip]
mod winit_web {
    use tracing::warn;

    pub struct MediaQueryList;

    impl MediaQueryList {
        pub fn media(&self) -> String {
            "(resolution: 1dppx)".to_owned()
        }
    }

    pub fn forms() {
        let scale = 2.0f64;
        let mql = MediaQueryList;
        // vendor/winit/src/platform_impl/web/web_sys/resize_scaling.rs:208
        warn!(
            "media query tracking scale factor was triggered without a change:\nMedia Query: \
             {}\nCurrent Scale: {scale}",
            mql.media(),
        );

        let location = 3u32;
        // vendor/winit/src/platform_impl/web/web_sys/event.rs:192
        tracing::warn!("Unexpected key location: {location}");

        let error = "decode failed";
        let failed = true;
        if failed {
            // vendor/winit/src/platform_impl/web/cursor.rs:272 — expression
            // position, no trailing semicolon.
            tracing::error!(
                "trying to load custom cursor that has failed to load: {error}"
            )
        }
    }
}

/// The iOS form is the one that proves the shim never assumes its first token
/// is a string literal: the format string is itself a `concat!` invocation,
/// assembled from the enclosing `macro_rules!`'s metavariables.
#[rustfmt::skip]
mod winit_ios {
    // vendor/winit/src/platform_impl/ios/window.rs:15
    use tracing::{debug, warn};

    pub struct Inner;

    impl Inner {
        pub fn set_title(&self, _title: &str) {
            // vendor/winit/src/platform_impl/ios/window.rs:118 — the macro is
            // the whole function body, in expression position, with NO trailing
            // semicolon. 20-odd iOS stubs are shaped exactly like this.
            debug!("`Window::set_title` is ignored on iOS")
        }

        pub fn is_visible(&self) -> Option<bool> {
            // vendor/winit/src/platform_impl/ios/window.rs:134
            warn!("`Window::is_visible` is ignored on iOS");
            None
        }
    }

    #[allow(non_snake_case)]
    pub struct NSOperatingSystemVersion {
        pub majorVersion: u32,
        pub minorVersion: u32,
        pub patchVersion: u32,
    }

    pub struct AppState {
        pub os_version: NSOperatingSystemVersion,
    }

    macro_rules! os_capabilities {
        ($objc_call:literal, $major:literal, $minor:literal) => {
            impl AppState {
                pub fn check(&self) {
                    let extra_msg = "Ignoring the call.";
                    // vendor/winit/src/platform_impl/ios/app_state.rs:869
                    tracing::warn!(
                        concat!("`", $objc_call, "` requires iOS {}.{}+. This device is running iOS {}.{}.{}. {}"),
                        $major, $minor, self.os_version.majorVersion, self.os_version.minorVersion, self.os_version.patchVersion,
                        extra_msg
                    )
                }
            }
        };
    }

    os_capabilities!("setNeedsUpdateOfHomeIndicatorAutoHidden", 11, 0);

    pub fn forms() {
        Inner.set_title("aterm");
        assert!(Inner.is_visible().is_none());

        AppState {
            os_version: NSOperatingSystemVersion {
                majorVersion: 10,
                minorVersion: 3,
                patchVersion: 1,
            },
        }
        .check();
    }
}

// ===========================================================================
// softbuffer 0.4.8 — always fully qualified, never imported
// ===========================================================================

#[rustfmt::skip]
mod softbuffer {
    pub struct WindowHandle {
        pub window: u32,
    }

    pub struct X11Impl {
        pub window: u32,
    }

    impl X11Impl {
        pub fn resize(&self, width: u32, height: u32) {
            // softbuffer-0.4.8/src/backends/x11.rs:314
            tracing::trace!(
                "resize: window={:X}, size={}x{}",
                self.window,
                width,
                height
            );
        }
    }

    pub fn forms() {
        // softbuffer-0.4.8/src/backends/x11.rs:97
        tracing::info!("no XCB connection provided by the user, so spawning our own");

        let window_handle = WindowHandle { window: 0x40_0001 };
        // softbuffer-0.4.8/src/backends/x11.rs:214
        tracing::trace!("new: window_handle={:X}", window_handle.window);

        X11Impl { window: 0x40_0001 }.resize(640, 480);

        // softbuffer-0.4.8/src/backends/x11.rs:437
        tracing::debug!("Falling back to non-SHM method for window drawing.");

        let name = "/softbuffer-shm";
        let i = 3u32;
        // softbuffer-0.4.8/src/backends/x11.rs:854
        tracing::warn!("x11: SHM ID collision at {} on try number {}", name, i);

        // softbuffer-0.4.8/src/backends/kms.rs:171
        tracing::warn!("no CRTC attached to plane, falling back to primary CRTC");
    }
}

// ===========================================================================
// tiny-xlib 0.2.5
// ===========================================================================

/// tiny-xlib is the only consumer that puts an ATTRIBUTE on the macro
/// statement itself. All five of its call sites sit under
/// `#[cfg(feature = "tracing")]` — three directly on the macro statement, two
/// inside a `#[cfg]`-attributed block — so the statement rustc has to accept is
/// not `mac!(…);` but `#[attr] mac!(…);`, whose expansion is `#[attr] {};`.
///
/// THE PREDICATE IS SPELT `all()` HERE, ON PURPOSE. Copying tiny-xlib's
/// `feature = "tracing"` verbatim would be worse than useless: this crate has
/// no `tracing` feature, so the predicate would be false, every statement below
/// would be stripped before it was ever parsed as a macro call, and the module
/// would prove nothing while looking like it proved everything. `all()` is the
/// always-true predicate, so the attributed-statement form is preserved and the
/// macro underneath it is really expanded.
#[rustfmt::skip]
mod tiny_xlib {
    // `clippy::non_minimal_cfg` is right about `all()` in ordinary code and
    // wrong here: `all()` is not a leftover from a condition that got deleted,
    // it is the ONLY spelling that is guaranteed true on every cell. Minimising
    // it away would delete the attribute, and the attribute is the thing under
    // test. See the module comment above for why the real predicate cannot be
    // copied verbatim.
    #![allow(clippy::non_minimal_cfg)]

    /// Every tiny-xlib handler is a plain `fn` pointer stored beside its key.
    type Handler = fn(&&u8, &&str) -> bool;

    /// tiny-xlib-0.2.5/src/lib.rs:221 — `unsafe extern "C" fn error_handler`.
    pub fn error_handler() {
        let display = 0u8;
        // A reference rather than tiny-xlib's raw pointer: the `&*` deref form
        // is what the shim has to swallow, and a reference exercises it without
        // importing a raw-pointer hazard into a test that must never evaluate.
        let display_ptr = &display;
        let event = "BadMatch";
        // tiny-xlib-0.2.5/src/lib.rs:260-265 — fields FIRST, message LAST, the
        // message itself comma-terminated, and the whole statement attributed.
        #[cfg(all())]
        tracing::error!(
            display = ?&*display_ptr,
            error = ?event,
            "got Xlib error",
        );

        let mut handlers: Vec<(usize, Handler)> = vec![(2, |_d, _e| true)];
        // tiny-xlib-0.2.5/src/lib.rs:268 — `_i` is a CLOSURE PATTERN BINDING
        // whose only mention is inside a macro argument. That is the
        // `unused_variables` divergence in its least obvious shape: unlike a
        // `let`, there is nowhere to move the binding to.
        handlers.iter_mut().any(|(_i, handler)| {
            // tiny-xlib-0.2.5/src/lib.rs:269-270
            #[cfg(all())]
            tracing::trace!(key = _i, "invoking error handler");

            let stop_going = (handler)(&display_ptr, &event);

            // tiny-xlib-0.2.5/src/lib.rs:274-281 — here the attribute is on a
            // BLOCK whose only contents are macro calls, so after expansion the
            // block is `{ if stop_going { {} } else { {} } }`. It still has to
            // type-check as a statement.
            #[cfg(all())]
            {
                if stop_going {
                    // tiny-xlib-0.2.5/src/lib.rs:277
                    tracing::trace!("error handler returned true, stopping");
                } else {
                    // tiny-xlib-0.2.5/src/lib.rs:279
                    tracing::trace!("error handler returned false, continuing");
                }
            }

            stop_going
        });
    }

    /// tiny-xlib-0.2.5/src/lib.rs:426 — an attributed macro statement that is
    /// NOT the last statement of its block: the closure still has to return the
    /// `0` underneath it.
    pub fn screen_index(index: i32) -> usize {
        // tiny-xlib-0.2.5/src/lib.rs:433
        index.try_into().unwrap_or_else(|_| {
            // tiny-xlib-0.2.5/src/lib.rs:434-437
            #[cfg(all())]
            tracing::error!(
                "XDefaultScreen returned a value out of usize range (how?!), returning zero"
            );
            0
        })
    }

    pub fn forms() {
        error_handler();
        assert_eq!(screen_index(3), 3);
        assert_eq!(screen_index(-1), 0);
    }
}

// ===========================================================================
// zbus 5.16.0 — the only consumer that uses spans, `Instrument` and
// `#[instrument]`, and the only one with a borrow hazard
// ===========================================================================

#[rustfmt::skip]
mod zbus_connection {
    // zbus-5.16.0/src/connection/mod.rs:15
    use tracing::{Instrument, debug, info_span, instrument, trace, trace_span, warn};

    use crate::executor_spawn;

    pub struct UniqueName;

    impl UniqueName {
        pub fn get(&self) -> Option<&'static str> {
            Some(":1.42")
        }
    }

    pub struct Inner {
        pub unique_name: UniqueName,
    }

    pub struct Event;

    pub struct Connection;

    impl Connection {
        pub async fn remove_match(&self, _rule: String) {}

        /// zbus-5.16.0/src/connection/mod.rs:959 — `#[instrument]` on a
        /// **non-`async`** function. Two of zbus's 23 sites are plain `fn`s
        /// (this one and `proxy/mod.rs:265`), and they are the ones that would
        /// break first if the pass-through ever started assuming an `async fn`
        /// body: upstream `#[instrument]` generates a *different* expansion for
        /// each, so "it works on the async ones" is not evidence about these.
        #[instrument(skip(self))]
        pub(crate) fn start_object_server(&self, started_event: Option<Event>) {
            let _ = started_event;
            // zbus-5.16.0/src/connection/mod.rs:962
            trace!("starting ObjectServer task");

            let stream: Result<(), &'static str> = Err("connection closed");
            if let Err(e) = stream {
                // zbus-5.16.0/src/connection/mod.rs:978
                debug!("Failed to create message stream: {}", e);
            }
        }
    }

    pub fn forms() {
        let well_known_name = "org.freedesktop.DBus";

        // zbus-5.16.0/src/connection/mod.rs:632 — message literal ALONE, with a
        // trailing comma after it.
        warn!(
            "Requesting name `{well_known_name}` before setting up the object server. \
                Method calls arriving before interfaces are registered may be lost. \
                Consider using `connection::Builder::serve_at()` and `::name()` instead.",
        );

        // zbus-5.16.0/src/connection/mod.rs:676 — bound to a local, moved into
        // `.instrument(…)` at line 720, ~44 lines later.
        let lost_task_name_span = info_span!("monitor_name_lost", name = %well_known_name);

        let inner = Inner { unique_name: UniqueName };
        let _lost_task = executor_spawn(
            async move {
                // zbus-5.16.0/src/connection/mod.rs:692 — note the comments
                // sitting between the format string and its arguments.
                tracing::info!(
                    "Connection `{}` lost name `{}`",
                    // SAFETY: This is bus connection so unique name can't be
                    // None.
                    inner.unique_name.get().unwrap(),
                    well_known_name
                );
            }
            // zbus-5.16.0/src/connection/mod.rs:720
            .instrument(lost_task_name_span),
        );

        let _obj_server_task = executor_spawn(
            async move { /* ObjectServer::dispatch loop */ }
                // zbus-5.16.0/src/connection/mod.rs:1040
                .instrument(info_span!("obj_server_task")),
        );

        Connection.start_object_server(Some(Event));
    }

    /// zbus-5.16.0/src/connection/mod.rs:1139 — THE borrow hazard.
    ///
    /// `task_name` is a `String` that the span macro appears to take by value,
    /// and that zbus goes on to use again on the very next expression. A shim
    /// that expanded to `let _ = task_name;` would move it and fail to compile;
    /// one that expanded to `let _ = &task_name;` would compile but evaluate.
    /// Discarding the tokens outright is the only expansion that does neither.
    pub fn borrow_hazard() -> String {
        let conn = Connection;
        let rule = "type='signal'".to_owned();
        let task_name = format!("remove match rule {rule}");

        let task =
            async move { conn.remove_match(rule).await }.instrument(trace_span!("{}", task_name));

        // `task_name` is STILL LIVE here, exactly as it is in zbus.
        let name = task_name.clone();
        crate::block_on(task);
        drop(task_name);
        name
    }
}

#[rustfmt::skip]
mod zbus_proxy {
    // zbus-5.16.0/src/proxy/mod.rs:16
    use tracing::{Instrument, debug, info_span, instrument, trace, warn};

    use crate::executor_spawn;

    pub struct Proxy;

    pub struct PropertiesCache;

    impl PropertiesCache {
        /// zbus-5.16.0/src/proxy/mod.rs:265 — the OTHER non-`async`
        /// `#[instrument]`, and the only one carrying `skip_all` on a plain
        /// `fn`. (An earlier revision of this file cited line 265 but pasted
        /// the attribute onto an `async fn`, which silently dropped the
        /// non-async shape from the oracle.)
        #[instrument(skip_all, level = "trace")]
        fn new(interface: &str) -> (Self, String) {
            let task_name = format!("{interface} proxy caching");
            (PropertiesCache, task_name)
        }

        // zbus-5.16.0/src/proxy/mod.rs:394 — the same attribute on an `async fn`.
        #[instrument(skip_all, level = "trace")]
        async fn keep_updated(&self) -> Result<(), ()> {
            // (synthetic) coverage for the imported `trace` and `debug`.
            trace!("resolving destination");
            debug!("resolved");
            Ok(())
        }
    }

    pub fn forms() {
        let task_name = format!("{} proxy signal handler", "org.freedesktop.DBus");
        let _proxy_task = executor_spawn(
            async move { /* signal stream */ }
                // zbus-5.16.0/src/proxy/mod.rs:320
                .instrument(info_span!("{}", task_name)),
        );

        // (synthetic) coverage for the imported `warn`.
        warn!("proxy: no destination");

        let (cache, name) = PropertiesCache::new("org.freedesktop.DBus.Properties");
        assert!(name.ends_with("proxy caching"));
        assert!(crate::block_on(cache.keep_updated()).is_ok());
        let _ = Proxy;
    }
}

#[rustfmt::skip]
mod zbus_socket_reader {
    // zbus-5.16.0/src/connection/socket_reader.rs:4
    use tracing::{debug, instrument, trace};

    pub struct SocketReader;

    #[derive(Debug)]
    pub struct Message;

    impl SocketReader {
        // zbus-5.16.0/src/connection/socket_reader.rs:47
        // (`single_match` allowed for the same reason as in `winit_wayland`:
        // the arm-in-expression-position shape is what is being tested.)
        #[allow(clippy::single_match)]
        #[instrument(name = "socket reader", skip(self), level = "trace")]
        pub async fn receive_msg(&mut self) {
            let received: Result<Message, ()> = Ok(Message);
            match received {
                // zbus-5.16.0/src/connection/socket_reader.rs:53
                Ok(msg) => trace!("Message received on the socket: {:?}", msg),
                Err(()) => {},
            }

            let rule = Some("type='signal'");
            let e = "channel closed";
            // zbus-5.16.0/src/connection/socket_reader.rs:65
            debug!("Error matching message against rule: {:?}", e);

            // zbus-5.16.0/src/connection/socket_reader.rs:82
            trace!(
                "Error broadcasting message to stream for `{:?}`: {:?}",
                rule, e
            )
        }

        // zbus-5.16.0/src/connection/socket_reader.rs:100
        #[instrument(skip(self), level = "trace")]
        pub async fn read_socket(&mut self) {}
    }

    pub fn forms() {
        let mut reader = SocketReader;
        crate::block_on(reader.receive_msg());
        crate::block_on(reader.read_socket());
    }
}

#[rustfmt::skip]
mod zbus_handshake {
    use tracing::{instrument, trace};

    pub type Result<T> = std::result::Result<T, ()>;

    #[derive(Debug)]
    pub struct Command;

    pub struct Handshake;

    impl Handshake {
        // zbus-5.16.0/src/connection/handshake/common.rs:60
        #[instrument(skip(self))]
        pub async fn write_command(&mut self, command: Command) -> Result<()> {
            Ok(())
        }

        // zbus-5.16.0/src/connection/handshake/server.rs:114
        #[instrument(skip(self))]
        pub async fn next_step(&mut self) -> Result<bool> {
            trace!("advancing the server handshake");
            Ok(true)
        }
    }

    pub fn forms() {
        let mut handshake = Handshake;
        assert!(crate::block_on(handshake.write_command(Command)).is_ok());
        assert_eq!(crate::block_on(handshake.next_step()), Ok(true));
    }
}

/// zbus's handshake client — the one module in the graph whose `#[instrument]`
/// is STACKED under another attribute.
#[rustfmt::skip]
mod zbus_handshake_client {
    // See `tiny_xlib` for why the always-true `cfg` predicate is spelt `all()`.
    #![allow(clippy::non_minimal_cfg)]

    // zbus-5.16.0/src/connection/handshake/client.rs:2
    use tracing::{instrument, trace, warn};

    pub type Result<T> = std::result::Result<T, ()>;

    #[derive(Debug)]
    pub enum Command {
        AgreeUnixFD,
        Error(&'static str),
    }

    pub struct Client;

    impl Client {
        /// zbus-5.16.0/src/connection/handshake/client.rs:61-63 — VERBATIM,
        /// `#[cfg]` and all.
        ///
        /// `#[instrument]` is the OUTER attribute, so the pass-through runs
        /// first and has to hand the `#[cfg]` back untouched for rustc to
        /// evaluate afterwards. A `#[instrument]` that rebuilt the item from a
        /// parsed AST could drop or reorder the inner attribute and nothing
        /// would notice until a FreeBSD build. On this machine the predicate is
        /// false and the item is stripped — *after* the attribute macro has
        /// already expanded, which is exactly the half being tested here.
        #[instrument(skip(self), level = "trace")]
        #[cfg(any(target_os = "freebsd", target_os = "dragonfly"))]
        async fn send_zero_byte(&mut self) -> Result<()> {
            trace!("sending zero byte");
            Ok(())
        }

        /// The same stacking with an always-true predicate, so the item
        /// survives on every cell and its body is really type-checked. Without
        /// this twin the line above would be a no-op on all four of aterm's
        /// targets.
        #[instrument(skip(self), level = "trace")]
        #[cfg(all())]
        async fn send_zero_byte_always(&mut self) -> Result<()> {
            trace!("sending zero byte");
            Ok(())
        }

        // zbus-5.16.0/src/connection/handshake/client.rs:85
        #[instrument(skip(self), level = "trace")]
        async fn authenticate(&mut self) -> Result<()> {
            let mechanism = "EXTERNAL";
            // zbus-5.16.0/src/connection/handshake/client.rs:88
            trace!("Trying {mechanism} mechanism");

            let cap_unix_fd = Command::Error("not supported");
            match cap_unix_fd {
                Command::AgreeUnixFD => {},
                // zbus-5.16.0/src/connection/handshake/client.rs:130 — a match
                // arm over an ENUM VARIANT PATTERN WITH A BINDING, where the
                // binding's only use is the macro's inline capture `{e}`.
                Command::Error(e) => warn!("UNIX file descriptor passing rejected: {e}"),
            }

            Ok(())
        }
    }

    pub fn forms() {
        let mut client = Client;
        assert!(crate::block_on(client.send_zero_byte_always()).is_ok());
        assert!(crate::block_on(client.authenticate()).is_ok());
    }
}

#[rustfmt::skip]
mod zbus_message_stream {
    use tracing::warn;

    pub fn forms() {
        let outcome: Result<(), &'static str> = Err("no such rule");
        match outcome {
            Ok(()) => {},
            // zbus-5.16.0/src/message_stream.rs:298
            Err(e) => warn!("Failed to remove match rule: {}", e),
        }
    }
}

#[rustfmt::skip]
mod zbus_object_server {
    // zbus-5.16.0/src/object_server/mod.rs:4
    use tracing::{Instrument, debug, instrument, trace, trace_span};

    use crate::executor_spawn;

    pub struct Message;

    impl std::fmt::Display for Message {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("MethodCall")
        }
    }

    pub struct ObjectServer;

    impl ObjectServer {
        // zbus-5.16.0/src/object_server/mod.rs:435
        #[instrument(skip(self))]
        pub(crate) async fn dispatch_call(&self, msg: &Message) -> Result<(), ()> {
            let dispatched: Result<(), &'static str> = Err("UnknownInterface");
            if let Err(e) = dispatched {
                // zbus-5.16.0/src/object_server/mod.rs:440
                debug!("Returning error: {}", e);
            }
            // zbus-5.16.0/src/object_server/mod.rs:443
            trace!("Handled: {}", msg);

            Ok(())
        }
    }

    pub fn forms() {
        assert!(crate::block_on(ObjectServer.dispatch_call(&Message)).is_ok());

        let msg = "MethodCall";
        let e = "UnknownInterface";
        // zbus-5.16.0/src/object_server/mod.rs:405
        debug!(
            "Error dispatching message. Message: {:?}, error: {:?}",
            msg, e
        );

        let task_name = format!("{} dispatcher", "object server");
        let _dispatch_task = executor_spawn(
            async move { /* dispatch */ }
                // zbus-5.16.0/src/object_server/mod.rs:412
                .instrument(trace_span!("{}", task_name)),
        );
        // As at connection/mod.rs:1139, `task_name` outlives the span macro.
        assert!(task_name.ends_with("dispatcher"));
    }
}

#[rustfmt::skip]
mod zbus_socket_unix {
    pub fn forms() {
        // zbus-5.16.0/src/connection/socket/unix.rs:379 — a FUNCTION-SCOPED
        // import, which `#![deny(unused_imports)]` scrutinises just as hard.
        use tracing::debug;

        debug!("fd passing is not supported on this transport");
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// Running every verbatim form. Compiling this file is the surface proof;
/// running it adds that nothing here panics, allocates wrongly, or diverges.
#[test]
fn every_consumer_form_compiles_and_runs() {
    winit_android_mod::forms();
    winit_wayland::forms();
    winit_macos_util::forms();
    winit_spans::forms();
    winit_macos_observer::forms();
    winit_macos::forms();
    winit_x11::forms();
    winit_x11::ime_forms(
        &winit_x11::ImeClientData {
            text: "aterm".to_owned(),
        },
        &winit_x11::ImeCallData {
            chg_first: 0,
            chg_length: 5,
        },
    );
    winit_windows::forms();
    winit_web::forms();
    winit_ios::forms();
    softbuffer::forms();
    tiny_xlib::forms();
    zbus_connection::forms();
    zbus_proxy::forms();
    zbus_socket_reader::forms();
    zbus_handshake::forms();
    zbus_handshake_client::forms();
    zbus_message_stream::forms();
    zbus_object_server::forms();
    zbus_socket_unix::forms();
}

/// zbus's `trace_span!("{}", task_name)` must not move `task_name`.
#[test]
fn span_macro_does_not_move_its_field_values() {
    assert_eq!(
        zbus_connection::borrow_hazard(),
        "remove match rule type='signal'"
    );
}

/// THE behavioural claim: a disabled callsite evaluates nothing, so this shim
/// must evaluate nothing.
///
/// The two controls at the top are not ceremony. Without them a tripwire that
/// silently stopped counting, or a `detonate` that silently stopped panicking,
/// would make every assertion below pass for the wrong reason.
#[test]
fn macros_never_evaluate_their_arguments() {
    // Control 1 — the counter is armed.
    EVALUATIONS.store(0, Ordering::SeqCst);
    assert_eq!(tripwire(7), 7);
    assert_eq!(
        EVALUATIONS.load(Ordering::SeqCst),
        1,
        "tripwire is not counting; nothing below this line would prove anything"
    );

    // Control 2 — the panic is armed. The hook is swapped out so the expected
    // panic does not scribble a backtrace across a passing test run.
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let detonated = panic::catch_unwind(AssertUnwindSafe(detonate));
    panic::set_hook(hook);
    assert!(
        detonated.is_err(),
        "detonate() no longer panics; the assertions below are vacuous"
    );

    // Now fire every macro at both tripwires, in every argument position the
    // grammar has: format string, positional argument, named field value with
    // and without a `?` / `%` sigil, and span fields.
    EVALUATIONS.store(0, Ordering::SeqCst);

    trace!("{}", tripwire("trace"));
    debug!(field = tripwire(1), "debug");
    info!(field = ?tripwire(2), "info");
    warn!(field = %tripwire(3), "warn");
    error!("{} {}", tripwire(4), detonate());
    trace!("{}", detonate());
    debug!(field = detonate(), "debug");
    info!(field = ?detonate(), "info");
    warn!(field = %detonate(), "warn");
    error!(detonate(), "error");

    let _s1 = trace_span!("span", field = tripwire(5));
    let _s2 = debug_span!("span", field = ?detonate());
    let _s3 = info_span!("{}", detonate());
    let _g = debug_span!("guard", field = %detonate()).entered();

    assert_eq!(
        EVALUATIONS.load(Ordering::SeqCst),
        0,
        "a tracing macro evaluated its arguments; the shim is no longer equivalent to a \
         disabled upstream callsite"
    );
}

/// `Instrumented<F>` must be `Future<Output = F::Output> + Send + 'static`
/// whenever `F` is, because zbus hands it straight to
/// `async_executor::Executor::spawn`.
#[test]
fn instrumented_future_is_spawnable_and_transparent() {
    use tracing::{Instrument, info_span};

    let future = async { 0xA7u8 }.instrument(info_span!("obj_server_task"));
    let future = executor_spawn(future);
    assert_eq!(block_on(future), 0xA7);
}

/// The wrapper must poll a `!Unpin` future correctly — a self-referential
/// `async` block is the shape zbus actually spawns, and it is what the manual
/// pin projection in the shim exists for.
#[test]
fn instrumented_future_polls_a_not_unpin_future() {
    use tracing::{Instrument, trace_span};

    let future = async {
        let value = 5u32;
        let borrowed = &value;
        std::future::ready(()).await;
        *borrowed * 2
    }
    .instrument(trace_span!("self_referential"));

    assert_eq!(block_on(future), 10);
}

/// `Span` is a real owned value with the guard API winit's ~90 sites use.
#[test]
fn span_types_behave_like_owned_values() {
    use tracing::Span;

    let span = Span::none();
    let clone = span.clone();
    assert_eq!(span, clone);

    let guard = span.entered();
    let recovered: Span = guard.exit();
    assert_eq!(recovered, clone);

    // The winit shape: bind the guard, let the scope end.
    let _span = tracing::debug_span!("winit::Window::id",).entered();
}

/// The public surface NO CONSUMER TOUCHES.
///
/// Everything above is a copy of something a real crate writes. This test is
/// the complement: the four items the shim exports that nothing in aterm's
/// dependency graph names. They are exported anyway — the shim's module docs
/// call `warn_span!` / `error_span!` "free insurance" against the next
/// dependency bump — and until this test existed they had **no coverage at
/// all**, in any test or doctest. Deleting `warn_span!` outright would have
/// left the whole suite green.
///
/// This is the one place in the file where "no consumer writes this" is the
/// reason a form is here rather than the reason it is not.
#[test]
fn unused_but_exported_surface_still_works() {
    use tracing::{Instrument, Span, error_span, warn_span};

    // The two span macros nothing calls, in both consumer-shaped positions.
    let _warn = warn_span!("aterm::insurance", field = ?"value");
    let _guard = error_span!("aterm::insurance", field = %"value").entered();

    // `Instrument::in_current_span` — the trait's second method. zbus only ever
    // calls `.instrument(span)`.
    let future = async { 3u8 }.in_current_span();
    // `Instrumented::span`, the accessor that keeps the stored `Span` field
    // from being `dead_code`.
    assert_eq!(future.span(), &Span::none());
    assert_eq!(block_on(future), 3);

    // `Span: Default`, which the derive provides and `Span::none` mirrors.
    // Spelt through the trait on purpose: `Span::default()` is what is being
    // asserted to exist, and clippy would otherwise "simplify" the call away
    // into the bare unit struct, deleting the assertion.
    assert_eq!(<Span as Default>::default(), Span::none());
}

/// The fully-qualified `#[tracing::instrument]` spelling.
///
/// Worth its own test because the shim's module docs assert that this path
/// resolves *and* attribute it to zbus — and zbus does not write it. There are
/// zero occurrences of `#[tracing::instrument` in all four consumer trees; both
/// zbus spellings go through `use tracing::instrument;`. The re-export claim is
/// still true and worth pinning, but nothing else in the graph pins it, so if
/// the `pub use` at the crate root ever moved, only this line would notice.
#[test]
fn fully_qualified_instrument_attribute_resolves() {
    #[tracing::instrument(skip_all, level = "trace")]
    fn annotated(value: u8) -> u8 {
        value * 2
    }

    assert_eq!(annotated(21), 42);
}

/// `Pin<&mut Instrumented<F>>` must not require `F: Unpin`; this is a
/// compile-level assertion about the bounds, and the `Box::pin` is the same
/// route `block_on` takes.
#[test]
fn instrumented_is_pin_projectable_without_unpin() {
    use tracing::{Instrument, Instrumented, debug_span};

    let future = async { 1u8 };
    let instrumented: Instrumented<_> = future.instrument(debug_span!("pinned"));
    let mut pinned: Pin<Box<Instrumented<_>>> = Box::pin(instrumented);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    assert_eq!(pinned.as_mut().poll(&mut cx), Poll::Ready(1u8));
}
