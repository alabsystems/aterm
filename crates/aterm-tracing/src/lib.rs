// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `tracing` — aterm's first-party replacement for the upstream `tracing`
//! facade. Every macro in it expands to nothing.
//!
//! This crate is published into the build under the package name `tracing` (the
//! directory is `crates/aterm-tracing`; see the manifest for why the two names
//! differ) so that `[patch.crates-io]` can redirect the four third-party
//! consumers in aterm's graph — the vendored winit fork, `softbuffer 0.4.8`,
//! `tiny-xlib 0.2.5` and `zbus 5.16.0` — at code we own. It replaces `tracing`,
//! `tracing-core` and `tracing-attributes` — and, on mac and windows only,
//! `pin-project-lite` too — with ~200 lines
//! that do nothing at all.
//!
//! # Why a no-op is *correct*, and why that is proved rather than argued
//!
//! The interesting claim is not "a no-op is good enough". It is that a no-op is
//! **behaviourally identical** to what ships today, and the identity rests on
//! three facts that can each be re-checked:
//!
//! 1. **aterm installs no subscriber.** Nothing in the workspace calls
//!    `set_global_default`, `set_default`, `with_default` or `init`, and no
//!    aterm crate depends on `tracing` at all — the only `tracing` mentions in
//!    workspace `.rs` files are prose in comments and one string literal.
//! 2. **`tracing-subscriber` is not in the graph, on any cell.** Re-check it:
//!
//!    ```text
//!    cargo tree --workspace -e all --target <cell> | grep -c tracing-subscriber   # => 0
//!    ```
//!
//!    USE `--workspace -e all`, NOT `-p aterm -e normal`. The narrow form was
//!    what this comment first recommended and it is too weak twice over: it
//!    omits DEV-dependencies, so a subscriber added to any workspace crate's
//!    `[dev-dependencies]` would keep the count at 0 while this shim silently
//!    blanked that test's output; and it looks at one cell. The wide form was
//!    run on all four cells and returns 0 on each, together with zero
//!    `tracing-log` — the other route by which events could reach real output.
//!
//!    (and `cargo tree --workspace --target all -i tracing` adds no consumer
//!    beyond the four above). No subscriber crate is *reachable*, let alone
//!    installed.
//! 3. **Therefore the global dispatcher is `NoSubscriber` for the whole process
//!    lifetime.** `tracing-core`'s dispatcher starts as `NoSubscriber`, and it
//!    only ever becomes anything else through the setters in (1). Against
//!    `NoSubscriber` every callsite's `Interest` resolves to *never*, so every
//!    event and span is disabled from the first invocation onward.
//!
//! The last link is the one that turns "disabled" into "identical": upstream
//! `tracing` **does not evaluate the arguments of a disabled callsite**. The
//! field values live inside a closure the macro only runs after the
//! enabled-check passes. A disabled `warn!(expensive(), "…")` never calls
//! `expensive()`.
//!
//! So the shipped behaviour of every `tracing` invocation in aterm's graph is:
//! evaluate nothing, allocate nothing, emit nothing. A macro that expands to
//! `{}` and evaluates its arguments **zero** times reproduces that exactly. It
//! is not an approximation with an accepted error term; the two programs do the
//! same thing.
//!
//! # The corollary: installing a subscriber means retiring this shim
//!
//! Fact (1) is a premise, not a law. The moment anyone adds a subscriber — a
//! `tracing-subscriber` dependency, a `tracing::subscriber::set_global_default`
//! call, an `init()` in a binary's `main` — the argument above collapses, and
//! this shim will silently produce **no output whatsoever**. It will not fail
//! to compile and it will not warn; the logs simply will not be there.
//!
//! If you want real tracing output, the fix is to *retire this crate*: drop the
//! `tracing` entry from the root `[patch.crates-io]` table so the real facade
//! resolves again. Do not try to grow this shim into a working one.
//!
//! # What "evaluates its arguments zero times" costs, and why it is still right
//!
//! Because the macros discard their token trees, a variable that a consumer
//! only ever mentions inside a `tracing!` argument becomes genuinely unused,
//! and rustc will say so. Two cases, and they are not the same:
//!
//! * **`use tracing::warn;` followed only by `warn!(…)` does NOT warn.** The
//!   import is still what *resolves* the macro name; name resolution happens
//!   before expansion, so the `use` is marked used and `unused_imports` stays
//!   quiet. `tests/consumer_forms.rs` pins this with a file-level
//!   `#![deny(unused_imports)]` over verbatim copies of the consumers' import
//!   lines — if that ever stopped being true, the test would stop compiling.
//! * **`let scale = compute(); info!("…{}", scale);` DOES warn** —
//!   `unused_variables`, because after expansion nothing mentions `scale`. This
//!   is the divergence, and it has exactly two other shapes: a method called
//!   only from a macro argument goes `dead_code`, and an `unsafe {}` block
//!   whose only unsafe operation was a macro argument goes `unused_unsafe`.
//! * **An ORDINARY import mentioned only inside a macro argument DOES warn** —
//!   `unused_imports`, and this is the case worth knowing because it looks like
//!   the first bullet and behaves like the second. `use std::ffi::CStr;` plus a
//!   lone `warn!("…", CStr::from_ptr(p))` leaves `CStr` genuinely unused. It
//!   costs nothing in practice — winit's `x11/mod.rs` is the only file in the
//!   graph shaped that way and it also calls `CStr::from_ptr` at line 998,
//!   outside any macro — but "the `use` is still used" is true of *macro*
//!   imports specifically, not of everything a macro argument mentions.
//!
//! It is tolerable because every affected file is third-party or vendored
//! (`vendor/winit`, and registry copies of softbuffer / tiny-xlib / zbus), none
//! of which is compiled with `-D warnings`; no first-party aterm crate uses
//! `tracing` at all, so no aterm file can be affected. The alternative — a
//! `let _ = &$value;` per field to keep the names "used" — was rejected
//! deliberately: taking a reference *is* evaluation. It would run
//! `unsafe { old.name() }` (winit's `window_delegate.rs:474`) on a disabled
//! callsite that upstream leaves untouched, which is precisely the behavioural
//! difference this crate exists not to have. Zero evaluation also disposes of
//! zbus's borrow hazard for free: `trace_span!("{}", task_name)` cannot move
//! `task_name`, because it does not touch it.
//!
//! # Surface
//!
//! Exactly what the four consumers use, and nothing else:
//!
//! * events — [`trace!`], [`debug!`], [`info!`], [`warn!`], [`error!`]
//! * spans — [`trace_span!`], [`debug_span!`], [`info_span!`], plus
//!   `warn_span!` / `error_span!` as free insurance
//! * types — [`Span`], [`EnteredSpan`], [`Instrument`], [`Instrumented`]
//! * attribute — `#[instrument]`, re-exported from `aterm-tracing-attributes`
//!   behind the default `attributes` feature, exactly as upstream re-exports
//!   `tracing-attributes`
//!
//! Deliberately **absent**, because nothing in the graph names them and adding
//! them half-way would be worse than not adding them: `Level`, `LevelFilter`,
//! `Metadata`, `Event`, `Id`, `Dispatch`, `Subscriber`, `field::*`, `Value`,
//! `WithSubscriber`, `tracing::log`, `level_filters::STATIC_MAX_LEVEL`, and the
//! `event!` / `span!` / `enabled!` / `event_enabled!` / `span_enabled!` macros
//! (all of which take a `Level` as their first argument — supplying the macro
//! without the type would only move the compile error). `Span::current()`,
//! `Span::enter()`, `Span::in_scope()` and `Span::record()` are absent for the
//! same reason: all 68 winit span sites use `.entered()`, on the spot, and not
//! one of them binds, records, exits or instruments the span.
//!
//! The `target:` / `parent:` colon-prefix macro forms are not implemented
//! either; no consumer uses them. Note that `target` and `name` DO appear as
//! ordinary field names via `=` (winit's `macos/util.rs`, zbus's
//! `#[instrument(name = …)]`), and those are covered — a field name is just a
//! token to this crate.
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
//!
//! # Proof of surface
//!
//! zbus, tiny-xlib and the winit Linux backends cannot be compiled on macOS, so
//! "it builds" is not available as evidence for most of the surface. What is
//! available is `tests/consumer_forms.rs`: every invocation form in the four
//! consumer trees, copied verbatim with a comment naming its file and line.
//! Compiling that test *is* the proof that this surface accepts what the real
//! consumers write, and it also asserts at runtime that an argument which would
//! panic or bump a counter does neither.

#![no_std]
// `no_std` unconditionally, and not behind the `std` feature. Measured with
// `cargo tree`, three of aterm's four cells (mac / windows / wasm) resolve this
// package with an EMPTY feature set — softbuffer and the winit fork both take
// it `default-features = false` — so every macro and every type here has to
// work with nothing turned on. Since none of them needs an allocator or an OS,
// the simplest way to guarantee that is to never link `std` at all and let the
// `std` feature be an accepted no-op. Integration tests and doctests are
// separate crates and get `std` as usual.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

/// The pass-through `#[instrument]` attribute.
///
/// Re-exported from the crate root under the default `attributes` feature.
///
/// zbus writes exactly ONE spelling — `use tracing::instrument;` followed by a
/// bare `#[instrument(skip_all, level = "trace")]` — at all 23 of its sites.
/// There is no `#[tracing::instrument]` anywhere in the four consumer trees.
/// The re-export still lives at the crate root because that is where upstream
/// `tracing` puts `tracing_attributes::instrument`, so the fully qualified path
/// keeps working for a future consumer. Since no consumer form pins it,
/// `consumer_forms.rs` pins that spelling in a test of its own.
#[cfg(feature = "attributes")]
pub use tracing_attributes::instrument;

// ---------------------------------------------------------------------------
// Spans
// ---------------------------------------------------------------------------

/// A span — always disabled, therefore always empty.
///
/// Upstream, a `Span` carries an optional `Id` plus the `Metadata` of its
/// callsite, and is the handle a subscriber uses to correlate events. With
/// `NoSubscriber` in force no id is ever issued, so the honest representation
/// of every span aterm creates is "nothing", and this is a zero-sized type.
///
/// It is still a real owned type rather than `()`, because the consumers treat
/// it as one: zbus binds a span to a local at `connection/mod.rs:676` and moves
/// it into `.instrument(…)` forty lines later, and winit calls `.entered()` on
/// one at 68 sites (counted, not estimated: `vendor/winit/src` holds exactly 68
/// `debug_span!` invocations and exactly 68 `.entered()` calls).
///
/// `Clone`/`Debug`/`PartialEq`/`Eq`/`Hash` match upstream's derives. `Copy` is
/// deliberately *not* derived: a `Copy` span would compile in places upstream
/// rejects, and this crate should never accept more than the real facade.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span;

impl Span {
    /// A span that does nothing — which, here, is all of them.
    ///
    /// This is the constructor every `*_span!` macro expands to. Upstream has
    /// the same name for the same meaning (a span with no id, attached to no
    /// subscriber); the difference is that upstream can also return other
    /// kinds.
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

    /// Enter the span, returning a guard that "exits" it when dropped.
    ///
    /// The only span-guard method aterm's graph uses (winit, 68 sites, always
    /// `let _span = …entered();`). It consumes the `Span` exactly as upstream
    /// does, so code that tries to use the span afterwards fails here the same
    /// way it would fail there.
    #[must_use]
    pub const fn entered(self) -> EnteredSpan {
        EnteredSpan
    }
}

/// The guard returned by [`Span::entered`].
///
/// Upstream this is `!Send` (it holds a `PhantomNotSend`) so a guard cannot be
/// held across an `.await` in a `Send` future without a compile error. This one
/// is a plain unit struct and therefore `Send`, which is *more* permissive than
/// upstream — the one place this crate is. Reproducing the negative impl would
/// mean carrying a `PhantomData<*const ()>` and the `unsafe impl Sync` that
/// upstream pairs with it, for a diagnostic no aterm code can trigger: nothing
/// first-party uses spans, and the 68 winit sites are all synchronous.
///
/// No `Drop` impl, because there is no span to exit. That also keeps
/// [`Instrumented`]'s pin projection sound-by-construction (see its `Future`
/// impl).
#[derive(Debug, Default)]
pub struct EnteredSpan;

impl EnteredSpan {
    /// Exit the span, handing back the [`Span`] it was entered from.
    ///
    /// Unused by any consumer; present because upstream has it and it costs one
    /// line, so a future caller does not have to discover the gap.
    #[must_use]
    pub const fn exit(self) -> Span {
        Span
    }
}

// ---------------------------------------------------------------------------
// Instrumenting futures
// ---------------------------------------------------------------------------

/// Attaches a [`Span`] to a future.
///
/// Blanket-implemented for every `Sized` type, exactly as upstream does, so
/// `use tracing::Instrument;` makes `.instrument(span)` available on anything.
/// zbus uses precisely one method of it — `.instrument(span)` on an async
/// block, whose result goes straight to `async_executor::Executor::spawn`.
pub trait Instrument: Sized {
    /// Wrap `self` so it is polled "inside" `span`.
    ///
    /// Since the span is disabled, the wrapper polls the inner future and does
    /// nothing else. The return type must be `Future<Output = Self::Output> +
    /// Send + 'static` whenever `Self` is, because zbus hands it to
    /// `Executor::spawn`; [`Instrumented`] gets all three properties
    /// automatically from its single field.
    fn instrument(self, span: Span) -> Instrumented<Self> {
        Instrumented { inner: self, span }
    }

    /// Wrap `self` with the current span.
    ///
    /// There is no "current" span without a subscriber, so this is
    /// `instrument(Span::none())`. Unused by any consumer; kept for parity.
    fn in_current_span(self) -> Instrumented<Self> {
        self.instrument(Span::none())
    }
}

impl<T: Sized> Instrument for T {}

/// A future with a [`Span`] attached — i.e. the future, unchanged.
///
/// Upstream needs `pin-project-lite` to build this type. We do the same
/// projection by hand in three lines, which is why the tracing -> ppl edge is
/// severed on EVERY cell.
///
/// That is not the same as ppl leaving the graph, and the difference was
/// measured rather than assumed: `pin-project-lite` disappears on mac-arm and
/// windows, but SURVIVES on linux (9 nodes, via zbus's async stack —
/// async-broadcast / async-channel / async-executor / async-lock /
/// event-listener / futures-lite) and on wasm (1 node, via
/// futures-util <- wasm-bindgen-futures <- winit). Neither has a `tracing`
/// parent; the package simply has other reasons to exist there. An earlier
/// draft of this comment claimed the removal unconditionally, which was true
/// of the cell it was written on and false of half the others.
#[derive(Debug, Clone)]
pub struct Instrumented<T> {
    // The only field with a size. Structurally pinned — see the `Future` impl.
    inner: T,
    // Zero-sized and never inspected, but stored rather than dropped on the
    // floor so the type keeps upstream's shape: the span lives exactly as long
    // as the wrapped future. Read by `span()` below, which is also what keeps
    // `dead_code` honest about it.
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
        // SAFETY: `inner` is structurally pinned in `Instrumented<T>`, and the
        // three obligations that makes are all discharged here:
        //   1. `inner` is never moved out of a pinned `Instrumented<T>`, and no
        //      `&mut T` to it escapes — this is the only place the field is
        //      touched at all (`span()` returns the other field).
        //   2. `Instrumented<T>` has no `Drop` impl, so no destructor can
        //      observe or relocate `inner` after it has been pinned. Neither
        //      does `Span` (a field-less unit struct).
        //   3. `Instrumented<T>: Unpin` is auto-derived and therefore holds
        //      only when `T: Unpin` — `Span` is `Unpin`, so `T` alone decides.
        //      A `!Unpin` inner future keeps the wrapper `!Unpin` too.
        let inner = unsafe { self.map_unchecked_mut(|this| &mut this.inner) };
        inner.poll(cx)
    }
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------
//
// Every macro below matches `($($discarded:tt)*)`. That single arm accepts any
// balanced token tree, which is the entire observed grammar and then some:
// bare literals, positional args with or without a trailing comma, inline
// format captures, string continuations, `concat!(…)` in the format position,
// `field = value`, `field = ?value`, `field = %value` (including `%unsafe { … }`
// and `?&*ptr`), shorthand `value` and `?value` captures, and fields before or
// after the message. Matching tokens rather than a grammar is not laziness —
// it is the direct expression of "we never inspect the arguments", and it is
// the only formulation that cannot be broken by a form nobody audited.
//
// The expansion is `{}`: an empty block. It has to be an expression, because
// these macros appear in expression position as well as statement position —
// `Err(e) => warn!("Failed to remove match rule: {}", e)` (zbus
// message_stream.rs:298) is a match arm whose value is the macro. `{}` is a
// valid expression of type `()` and a valid statement, so one expansion covers
// both. It also evaluates nothing, which is the point.
//
// All of them are `#[macro_export]`, which puts them at the crate root. In
// edition 2018+ that makes BOTH spellings work — `tracing::warn!(…)` fully
// qualified (winit's `x11/ime/context.rs:83`) and a bare `warn!(…)` after
// `use tracing::warn;` (winit's `wayland/window/state.rs:8`).

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
///
/// The expansion is a call expression, so the two ways consumers consume a
/// span both parse: `trace_span!("…").entered()` (method call directly on the
/// invocation) and `.instrument(trace_span!("{}", task_name))` (passed as an
/// argument). The second form is also the crate's one borrow hazard — zbus
/// uses `task_name` again after this line — and discarding the tokens means the
/// `String` is never moved.
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
/// evaluated.
///
/// Not used by any consumer today. Included — like `error_span!` — because the
/// body is one line and the alternative is re-auditing four dependency trees
/// the next time one of them bumps.
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
