// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `tracing` — aterm's first-party, no-op replacement for the upstream
//! `tracing` facade. Every macro expands to nothing.
//!
//! Published under the package name `tracing` so `[patch.crates-io]` redirects
//! the four third-party consumers — the vendored winit fork, `softbuffer`,
//! `tiny-xlib` and `zbus` — here. It replaces `tracing`, `tracing-core` and
//! `tracing-attributes` (and `pin-project-lite` on mac and windows).
//!
//! # Why a no-op is behaviourally identical
//!
//! 1. **aterm installs no subscriber.** Nothing in the workspace calls
//!    `set_global_default`, `set_default`, `with_default` or `init`, and no
//!    aterm crate depends on `tracing`.
//! 2. **No subscriber crate is in the graph, on any cell.** Re-check with
//!
//!    ```text
//!    cargo tree --workspace -e all --target <cell> | grep -cE 'tracing-(subscriber|log)'   # => 0
//!    ```
//!
//!    `--workspace -e all` matters: `-p aterm -e normal` misses
//!    dev-dependencies, so a subscriber added to a test would go unseen while
//!    this shim blanked its output. `cargo tree --workspace --target all -i
//!    tracing` names no consumer beyond the four above.
//! 3. **So the dispatcher is `NoSubscriber` for the whole process**, every
//!    callsite's `Interest` is *never*, and upstream does not evaluate the
//!    arguments of a disabled callsite (they live in a closure run only after
//!    the enabled-check).
//!
//! Shipped behaviour is therefore: evaluate nothing, allocate nothing, emit
//! nothing. A macro that expands to `{}` and evaluates its arguments zero times
//! reproduces that exactly.
//!
//! # Installing a subscriber means retiring this shim
//!
//! Fact (1) is a premise. Once anyone installs a subscriber, this shim silently
//! produces no output — no compile error, no warning. For real tracing output,
//! drop the `tracing` entry from the root `[patch.crates-io]` table; do not grow
//! this shim into a working one.
//!
//! # The cost of zero evaluation
//!
//! Discarding the token trees makes anything mentioned only inside a macro
//! argument genuinely unused:
//!
//! * `use tracing::warn;` followed only by `warn!(…)` does NOT warn — the
//!   import resolves the macro name before expansion.
//! * A variable, method, `unsafe {}` block or ordinary import used only in a
//!   macro argument DOES warn (`unused_variables`, `dead_code`,
//!   `unused_unsafe`, `unused_imports`).
//!
//! Every affected file is third-party (`vendor/winit` and registry copies),
//! none built with `-D warnings`. A `let _ = &$value;` per field was rejected:
//! taking a reference is evaluation, and would run e.g. winit's
//! `unsafe { old.name() }` on a callsite upstream leaves untouched. Zero
//! evaluation also means `trace_span!("{}", task_name)` never moves
//! `task_name` (zbus reuses it).
//!
//! # Surface
//!
//! Exactly what the four consumers use:
//!
//! * events — [`trace!`], [`debug!`], [`info!`], [`warn!`], [`error!`]
//! * spans — [`trace_span!`], [`debug_span!`], [`info_span!`], `warn_span!`,
//!   `error_span!`
//! * types — [`Span`], [`EnteredSpan`], [`Instrument`], [`Instrumented`]
//! * attribute — `#[instrument]`, re-exported from `aterm-tracing-attributes`
//!   behind the default `attributes` feature, as upstream does
//!
//! Deliberately absent, because no consumer names them: `Level`,
//! `LevelFilter`, `Metadata`, `Event`, `Id`, `Dispatch`, `Subscriber`,
//! `field::*`, `Value`, `WithSubscriber`, `tracing::log`,
//! `level_filters::STATIC_MAX_LEVEL`, the `Level`-taking `event!` / `span!` /
//! `*enabled!` macros, `Span::{current, enter, in_scope, record}` (winit only
//! ever calls `.entered()`), and the `target:` / `parent:` macro prefixes.
//! `target = …` and `name = …` as ordinary fields are covered: a field name is
//! just a token here.
//!
//! # Examples
//!
//! Everything below compiles, runs, and does nothing — which is the contract.
//!
//! ```
//! use tracing::{Instrument, Span, info_span, warn};
//!
//! // An event. `expensive()` is never called, exactly as on a disabled
//! // upstream callsite.
//! fn expensive() -> u64 { unreachable!("a disabled callsite evaluates nothing") }
//! warn!(cost = expensive(), "budget exceeded");
//!
//! // A span guard, the winit shape.
//! let _span = tracing::debug_span!("aterm::Window::set_title", title = "aterm").entered();
//!
//! // A span attached to a future, the zbus shape. The result is
//! // `Future + Send + 'static`, so it can go straight to an executor.
//! let task = async { 7u8 }.instrument(info_span!("obj_server_task"));
//! fn spawnable<F: core::future::Future + Send + 'static>(f: F) -> F { f }
//! let task = spawnable(task);
//!
//! assert_eq!(Span::none(), Span::default());
//! ```

#![no_std]
// Unconditionally, not behind the `std` feature: mac, windows and wasm resolve
// this package with NO features (softbuffer and winit take it
// `default-features = false`), and nothing here needs an allocator or an OS.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

/// The pass-through `#[instrument]` attribute, at the crate root where upstream
/// puts it (zbus spells it `use tracing::instrument;`).
#[cfg(feature = "attributes")]
pub use tracing_attributes::instrument;

/// A span — always disabled, therefore zero-sized and empty.
///
/// A real owned type rather than `()` because consumers treat it as one: zbus
/// binds a span and later moves it into `.instrument(…)`, winit calls
/// `.entered()`. The derives match upstream's; `Copy` is deliberately absent so
/// this never accepts code the real facade rejects.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span;

impl Span {
    /// A span that does nothing — here, all of them. Every `*_span!` macro
    /// expands to this.
    ///
    /// ```
    /// let span = tracing::Span::none();
    /// let moved_later = span;            // a real owned value, as zbus needs
    /// let _guard = moved_later.entered();
    /// ```
    #[must_use]
    pub const fn none() -> Self {
        Span
    }

    /// Enter the span, returning a guard. Consumes the `Span`, as upstream does.
    #[must_use]
    pub const fn entered(self) -> EnteredSpan {
        EnteredSpan
    }
}

/// The guard returned by [`Span::entered`].
///
/// Upstream's guard is `!Send`; this one is `Send`, the one place this crate is
/// more permissive. Reproducing it would need `PhantomData<*const ()>` plus an
/// `unsafe impl Sync` for a diagnostic no consumer can trigger (every winit
/// span site is synchronous).
///
/// No `Drop` impl: there is no span to exit.
#[derive(Debug, Default)]
pub struct EnteredSpan;

impl EnteredSpan {
    /// Exit the span, handing back the [`Span`] it was entered from.
    #[must_use]
    pub const fn exit(self) -> Span {
        Span
    }
}

/// Attaches a [`Span`] to a future. Blanket-implemented for every `Sized`
/// type, as upstream does.
pub trait Instrument: Sized {
    /// Wrap `self` so it is polled "inside" `span`. The wrapper is
    /// `Future + Send + 'static` whenever `Self` is, because zbus hands it to
    /// `Executor::spawn`.
    fn instrument(self, span: Span) -> Instrumented<Self> {
        Instrumented { inner: self, span }
    }

    /// Wrap `self` with the current span — with no subscriber, `Span::none()`.
    fn in_current_span(self) -> Instrumented<Self> {
        self.instrument(Span::none())
    }
}

impl<T: Sized> Instrument for T {}

/// A future with a [`Span`] attached — i.e. the future, unchanged.
///
/// The pin projection is done by hand, so this crate needs no
/// `pin-project-lite`. That package still survives on linux (zbus's async
/// stack) and wasm (`futures-util` via winit), just not through `tracing`.
#[derive(Debug, Clone)]
pub struct Instrumented<T> {
    // Structurally pinned — see the `Future` impl.
    inner: T,
    // Kept so the span lives exactly as long as the future, as upstream;
    // read by `span()`, which keeps `dead_code` quiet about it.
    span: Span,
}

impl<T> Instrumented<T> {
    /// The span attached to this future.
    #[must_use]
    pub const fn span(&self) -> &Span {
        &self.span
    }
}

impl<T: Future> Future for Instrumented<T> {
    type Output = T::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `inner` is structurally pinned:
        //   1. it is never moved out, and no `&mut T` to it escapes — this is
        //      the only place it is touched (`span()` reads the other field);
        //   2. neither `Instrumented<T>` nor `Span` has a `Drop` impl, so no
        //      destructor can relocate it;
        //   3. `Instrumented<T>: Unpin` is auto-derived and `Span` is `Unpin`,
        //      so the wrapper is `Unpin` exactly when `T` is.
        let inner = unsafe { self.map_unchecked_mut(|this| &mut this.inner) };
        inner.poll(cx)
    }
}

// Every macro matches `($($discarded:tt)*)`: any balanced token tree, so no
// argument form (format captures, `field = ?value`, `%unsafe { … }`, fields
// before or after the message) can break it — the arguments are never
// inspected.
//
// Event macros expand to `{}`, which is both a statement and a `()`
// expression; zbus uses `Err(e) => warn!(…)` as a match arm value. Span
// macros expand to a `Span::none()` call, so both `.entered()` on the
// invocation and passing it as an argument parse.
//
// `#[macro_export]` puts them at the crate root, so both `tracing::warn!(…)`
// and `use tracing::warn; warn!(…)` work.

/// A `TRACE`-level event. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! trace {
    ($($discarded:tt)*) => {{}};
}

/// A `DEBUG`-level event. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! debug {
    ($($discarded:tt)*) => {{}};
}

/// An `INFO`-level event. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! info {
    ($($discarded:tt)*) => {{}};
}

/// A `WARN`-level event. Expands to `{}`; arguments are never evaluated.
///
/// ```
/// use tracing::warn;
///
/// fn never_called() -> &'static str { unreachable!() }
///
/// // Bare message, positional args, inline captures, named fields with and
/// // without a sigil, shorthand captures — all accepted, none evaluated.
/// let code = 0x8000_4005u32;
/// warn!("HRESULT 0x{:X}", code);
/// warn!("locale {code:#?} unsupported");
/// warn!(code, reason = never_called(), detail = ?never_called(), "give up");
/// ```
#[macro_export]
macro_rules! warn {
    ($($discarded:tt)*) => {{}};
}

/// An `ERROR`-level event. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! error {
    ($($discarded:tt)*) => {{}};
}

/// A `TRACE`-level span. Expands to [`Span::none()`]; arguments are never
/// evaluated.
#[macro_export]
macro_rules! trace_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}

/// A `DEBUG`-level span. Expands to [`Span::none()`]; arguments are never
/// evaluated.
#[macro_export]
macro_rules! debug_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}

/// An `INFO`-level span. Expands to [`Span::none()`]; arguments are never
/// evaluated.
#[macro_export]
macro_rules! info_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}

/// A `WARN`-level span. Expands to [`Span::none()`]; arguments are never
/// evaluated. Unused today; kept so a consumer bump needs no re-audit.
#[macro_export]
macro_rules! warn_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}

/// An `ERROR`-level span. Expands to [`Span::none()`]; arguments are never
/// evaluated.
#[macro_export]
macro_rules! error_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}
