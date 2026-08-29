// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ORACLE: every `profiling` invocation form in aterm's dependency graph,
//! copied verbatim from the consumer that writes it, plus a differential
//! against upstream's own empty backend.
//!
//! # Why this file exists
//!
//! `[patch.crates-io]` redirects wgpu, wgpu-core and wgpu-hal at this crate,
//! and a large part of what it redirects cannot be compiled on the machine that
//! develops it: 27 of wgpu-hal's 60 call sites live in `src/dx12/`, behind
//! `#[cfg(dx12)]`, which no macOS build ever configures — and the one
//! multi-line single-argument form in the whole graph is one of them. "The
//! build is green" is therefore not evidence about the surface.
//!
//! So the proof is moved somewhere that compiles on every cell. A
//! paren-balancing scan of all 198 invocations closes the arity set at
//! `{1: 194, 2: 4}` in exactly four structural shapes — 1-arg single-line 193,
//! 1-arg multi-line 1, 2-arg single-line 2, 2-arg multi-line 2 — and each of
//! the four appears below, byte-for-byte, with a comment naming the crate, file
//! and line it came from. Only the surrounding scaffolding (the locals, structs
//! and methods the arguments mention) is written here. **Compiling this test is
//! the proof that the surface accepts what the real consumers write.** When a
//! consumer bumps and introduces a form this shim cannot parse, the fix is to
//! paste the new line in here first and watch it fail.
//!
//! # The three invariants, all mechanically re-derivable
//!
//! 1. **The surface is one macro.** A name histogram over the three consumer
//!    trees finds `profiling::scope!` and nothing else:
//!    `grep -rho 'profiling::[a-zA-Z_]*' <registry>/{wgpu,wgpu-core,wgpu-hal}-29.0.3
//!    --include='*.rs' | sort | uniq -c` => `198 profiling::scope`.
//! 2. **Every shape appears here.** Shape means token classes, not
//!    identifiers: `scope!(LIT)` and `scope!(LIT, LIT)` are two shapes, and
//!    `scope!(LIT, EXPR)` spread over three lines is a third.
//! 3. **Every site is a statement.** The character following the closing paren
//!    is `;` at all 198 of them, none is a block's trailing expression, and
//!    none sits inside a `macro_rules!` transcriber. That is what makes an
//!    empty expansion — rather than `aterm-tracing`'s `{}` — safe here.
//!
//! # The differential, and the dev-dependency that would have been a lie
//!
//! The in-repo pattern for a replacement crate (`crates/aterm-grapheme`,
//! `crates/aterm-hash`) is to keep the upstream crate as a `[dev-dependencies]`
//! and assert the two agree — invisible to the shipped graph, so it is free.
//! **That does not work for a `[patch.crates-io]` target, and the failure is
//! silent.** Measured, in a throwaway workspace shaped exactly like this one:
//!
//! ```text
//! # package `profiling` 1.0.18 at ./p, with
//! #   [dev-dependencies] upstream-profiling = { package = "profiling", version = "1.0.18" }
//! $ cargo tree                       # no patch
//! profiling v1.0.18 (…/p)
//! [dev-dependencies]
//! └── profiling v1.0.18              # the registry crate — a real differential
//!
//! $ cargo tree                       # with [patch.crates-io] profiling = { path = "p" }
//! profiling v1.0.18 (…/p)
//! [dev-dependencies]
//! └── profiling v1.0.18 (…/p) (*)    # ITSELF
//! ```
//!
//! The patch rewrites the dev-dependency too, so the day the patch row lands
//! the differential would quietly become this crate compared against this
//! crate: green forever, and worth nothing. An unarmed tripwire is worse than
//! no tripwire.
//!
//! What is honest instead is [`upstream_empty_impl`]: upstream's four macro
//! definitions, copied verbatim out of `profiling-1.0.18/src/empty_impl.rs`
//! (matchers and empty transcribers, byte for byte) into a module of this test,
//! where no patch can alias them away. Every consumer form below is fired
//! through BOTH, and both are held to evaluating nothing. Re-derive the copy
//! with `sed -n '/macro_rules/,$p' <registry>/profiling-1.0.18/src/empty_impl.rs`.
//! Its limit is worth stating: it is a copy, not an independent implementation,
//! so it cannot catch an error the copy shares. It does catch the two that
//! matter — a divergence in what the arms ACCEPT, and any evaluation of an
//! argument.
//!
//! # The runtime half, and the control that arms it
//!
//! Accepting the syntax is half the contract; evaluating nothing is the other
//! half, and it is what makes this shim identical to upstream rather than
//! merely quiet. Two tripwires pin it — a counter and a panic — and
//! [`tripwires_are_live`] proves both fire when they should BEFORE any test
//! asserts they did not. Without that control, "the counter is still zero"
//! would be a statement about the counter, not about the macros.
//!
//! # What the lint attribute below is evidence of
//!
//! `#![deny(unused_variables, unused_imports, dead_code)]` is an assertion, not hygiene.
//! wgpu-core `src/instance.rs:467-468` carries the comment "We might be using
//! `profiling` without any features. The empty backend of this macro emits no
//! code, so unused code linting changes depending on the backend" — so the
//! question "does expanding to nothing make a consumer's binding unused?" is
//! one upstream itself flagged. It was answered by enumerating the arguments:
//! of all 198 invocations only THREE contain a non-literal argument, and they
//! mention exactly two bindings — `_backend` (already underscore-prefixed at
//! `instance.rs:469`, so no lint can fire) and `pass` (used on the very next
//! line at `render.rs:1820` / `compute.rs:473`). Every verbatim copy below
//! therefore stands under a `deny`, and stays there: the mirror-image risk is
//! the interesting one — a shim that DID evaluate its arguments would silently
//! mark such bindings used and mask a future upstream slip.

#![deny(unused_variables, unused_imports, dead_code)]

extern crate alloc;

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

// ===========================================================================
// Tripwires — the runtime half of the contract
// ===========================================================================

/// Bumped once per real evaluation of [`tripwire`].
static EVALUATIONS: AtomicUsize = AtomicUsize::new(0);

/// An identity function with an observable side effect.
///
/// Placed in macro argument positions. If either shim ever evaluated its
/// arguments, the counter would move.
fn tripwire<T>(value: T) -> T {
    EVALUATIONS.fetch_add(1, Ordering::SeqCst);
    value
}

/// An argument that destroys the test if it is ever evaluated.
///
/// The counter alone would catch an accidental evaluation; this catches the
/// `let _ = &$name;` shape specifically, where taking a reference still calls
/// the function.
fn detonate() -> &'static str {
    panic!("a profiling macro evaluated its argument");
}

// ===========================================================================
// The differential half: upstream's own empty backend, frozen
// ===========================================================================

/// `profiling-1.0.18/src/empty_impl.rs`, verbatim.
///
/// The four macro definitions exactly as upstream writes them — same matchers,
/// same empty transcribers. `#[macro_export]` is dropped (it would hoist these
/// to this test crate's root and collide with the names under test); the
/// `pub(crate) use` below gives them a path instead, so every call site can
/// spell `crate::upstream_empty_impl::scope!(…)` beside `profiling::scope!(…)`
/// and the two can be compared line for line. (The `crate::` prefix is not
/// decoration: the consumer modules below are children of this crate root, and
/// a bare first segment there would be looked up as an external crate.)
#[rustfmt::skip]
mod upstream_empty_impl {
    macro_rules! scope {
        ($name:expr) => {};
        ($name:expr, $data:expr) => {};
    }

    macro_rules! function_scope {
        () => {};
        ($data:expr) => {};
    }

    macro_rules! register_thread {
        () => {};
        ($name:expr) => {};
    }

    macro_rules! finish_frame {
        () => {};
    }

    // Four single-segment `use`s rather than one braced group: a bare
    // `use scope;` is the documented way to give a `macro_rules!` a path, and
    // the braced form would resolve its members from the crate root instead.
    pub(crate) use finish_frame;
    pub(crate) use function_scope;
    pub(crate) use register_thread;
    pub(crate) use scope;
}

// ===========================================================================
// wgpu 29.0.3
// ===========================================================================

// THE `rustfmt::skip` ON EVERY CONSUMER MODULE BELOW IS LOAD-BEARING.
//
// These invocations are byte-for-byte copies of third-party source, and their
// exact shape — where the line breaks fall, how the continuation is indented,
// which trailing commas are absent — is part of what is being tested. rustfmt
// would happily fold the three-line dx12 form onto one line, at which point the
// file would still compile and would no longer be evidence of anything.

#[rustfmt::skip]
mod wgpu_api_queue {
    pub fn forms() {
        // wgpu-29.0.3/src/api/queue.rs:201 — the whole of wgpu's usage: one
        // 1-argument single-line site, the shape 193 of the 198 share.
        profiling::scope!("Queue::write_buffer_with");

        // The same line through upstream's frozen empty backend.
        crate::upstream_empty_impl::scope!("Queue::write_buffer_with");
    }
}

// ===========================================================================
// wgpu-hal 29.0.3
// ===========================================================================

#[rustfmt::skip]
mod wgpu_hal_backends {
    pub fn forms() {
        // wgpu-hal-29.0.3/src/metal/mod.rs:146 — 1-argument, single line. The
        // one wgpu-hal site this machine can actually compile.
        profiling::scope!("Init Metal Backend");
        crate::upstream_empty_impl::scope!("Init Metal Backend");

        // wgpu-hal-29.0.3/src/dx12/mod.rs:1398 — THE 1-argument MULTI-LINE
        // form, and the only one in the graph. It sits under `#[cfg(dx12)]`,
        // so no macOS or Linux build has ever parsed it; this copy is the only
        // place it is checked outside a Windows CI run.
        profiling::scope!(
                            "IDXGIFactoryMedia::CreateSwapChainForCompositionSurfaceHandle"
                        );
        crate::upstream_empty_impl::scope!(
                            "IDXGIFactoryMedia::CreateSwapChainForCompositionSurfaceHandle"
                        );
    }
}

// ===========================================================================
// wgpu-core 29.0.3 — the only crate with 2-argument sites
// ===========================================================================

#[rustfmt::skip]
mod wgpu_core_two_arg {
    /// Stands in for `wgpu_types::Backend`, which `instance.rs:469` formats.
    #[derive(Debug)]
    pub enum Backend { Vulkan }

    pub fn forms() {
        // wgpu-core-29.0.3/src/device/global.rs:2111 — 2 arguments, both
        // literals. `format!("unmap", "Buffer")` would be a compile error
        // ("argument never used"), which is the proof that `$name` and `$data`
        // are two independent fields and not a format string plus its argument.
        profiling::scope!("unmap", "Buffer");
        crate::upstream_empty_impl::scope!("unmap", "Buffer");

        // wgpu-core-29.0.3/src/instance.rs:469 — 2 arguments, the second an
        // allocating expression. `_backend` is the loop binding of
        // `self.instance_per_backend.iter()`, underscore-prefixed BY UPSTREAM
        // precisely because the empty backend leaves it unused (see the comment
        // at instance.rs:467-468). Nothing here allocates: the `alloc::format!`
        // is never expanded into a value.
        let _backend = Backend::Vulkan;
        profiling::scope!("enumerating", &*alloc::format!("{_backend:?}"));
        crate::upstream_empty_impl::scope!("enumerating", &*alloc::format!("{_backend:?}"));
    }
}

// ===========================================================================
// The borrowck shape — the reason this crate expands to nothing
// ===========================================================================

/// `wgpu-core-29.0.3`'s two multi-line 2-argument sites, in their real
/// borrowing context.
///
/// This module is not about the macro's grammar; it is about the borrow. Both
/// functions hand `pass.base.label` — a shared borrow of `pass.base` — to the
/// macro, and then take that same field mutably (`let base = pass.base.take();`
/// at render.rs:1830 / compute.rs:483). Whether that compiles depends entirely
/// on how long the expansion keeps the borrow alive.
///
/// HOW THIS WAS ARMED. Compiling the module is only evidence if the compile
/// could have failed, so four transcribers were built against this exact file
/// (`rustc --test consumer_forms.rs --extern profiling=<variant>`):
///
/// | expansion of the 2-argument arm | result |
/// |---|---|
/// | nothing — this crate, and upstream's empty backend | compiles |
/// | `let _: &str = $data;` — upstream's `type-check` backend | compiles |
/// | `let _data = $data;` — a plain named binding | compiles |
/// | `let _g = Guard($name, $data);` where `Guard: Drop` | fails, twice |
///
/// The failure is `error[E0502]: cannot borrow pass.base as mutable because it
/// is also borrowed as immutable`, once for `render_pass_end` and once for
/// `compute_pass_end`. Note which rows compile: a named binding does NOT hold
/// the borrow, because NLL ends it at the binding's last use and there is none.
/// The rule this file actually pins is narrower and truer — an expansion whose
/// value OUTLIVES the statement breaks wgpu-core, and an empty one cannot.
#[rustfmt::skip]
mod wgpu_core_borrow_shape {
    pub struct Base { pub label: Option<String> }
    impl Base {
        /// Stands in for `BasePass::take`, the statement wgpu-core runs eleven
        /// lines after the macro (`let base = pass.base.take();`,
        /// render.rs:1830 / compute.rs:483). It is the MUTABLE use of
        /// `pass.base` that a live borrow of `pass.base.label` collides with.
        pub fn take(&mut self) -> Base { Base { label: self.label.take() } }
    }
    pub struct Parent;
    pub struct RenderPass { pub base: Base, pub parent: Option<Parent> }
    pub struct ComputePass { pub base: Base, pub parent: Option<Parent> }

    /// wgpu-core-29.0.3/src/command/render.rs:1819-1830, condensed to the three
    /// statements that interact — the macro, `pass.parent.take()`, and
    /// `pass.base.take()`. The error handling between them is elided; the
    /// borrows are not.
    pub fn render_pass_end(pass: &mut RenderPass) -> Option<(Parent, Base)> {
        profiling::scope!(
            "CommandEncoder::run_render_pass {}",
            pass.base.label.as_deref().unwrap_or("")
        );

        let cmd_enc = pass.parent.take()?;
        let base = pass.base.take();
        Some((cmd_enc, base))
    }

    /// wgpu-core-29.0.3/src/command/compute.rs:472-483, likewise.
    pub fn compute_pass_end(pass: &mut ComputePass) -> Option<(Parent, Base)> {
        profiling::scope!(
            "CommandEncoder::run_compute_pass {}",
            pass.base.label.as_deref().unwrap_or("")
        );

        let cmd_enc = pass.parent.take()?;
        let base = pass.base.take();
        Some((cmd_enc, base))
    }

    /// The same shape through upstream's frozen backend, so the borrow result
    /// is differential rather than self-reported.
    pub fn upstream_render_pass_end(pass: &mut RenderPass) -> Option<(Parent, Base)> {
        crate::upstream_empty_impl::scope!(
            "CommandEncoder::run_render_pass {}",
            pass.base.label.as_deref().unwrap_or("")
        );

        let cmd_enc = pass.parent.take()?;
        let base = pass.base.take();
        Some((cmd_enc, base))
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// THE CONTROL. Nothing below is worth anything until this passes.
///
/// Both tripwires are proved live: the counter reaches 1 when `tripwire` is
/// called for real, and `detonate` really does unwind. Run first, and asserted
/// on rather than assumed, because "the counter never moved" is otherwise a
/// claim about a counter that may simply be broken.
#[test]
fn tripwires_are_live() {
    let before = EVALUATIONS.load(Ordering::SeqCst);
    let value = tripwire("armed");
    assert_eq!(value, "armed");
    assert_eq!(
        EVALUATIONS.load(Ordering::SeqCst),
        before + 1,
        "the evaluation counter must move on a direct call, or every \
         'arguments were not evaluated' assertion below is vacuous"
    );

    // The hook is swapped out so the expected panic does not scribble a
    // backtrace across a passing run.
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let detonated = panic::catch_unwind(AssertUnwindSafe(detonate));
    panic::set_hook(hook);
    assert!(
        detonated.is_err(),
        "detonate() no longer panics; every assertion below would be vacuous"
    );
}

/// Neither this shim nor upstream's empty backend evaluates an argument.
///
/// Every arm of every macro, fired at both tripwires at once. The counter is
/// read before and after so a concurrently-running test that legitimately calls
/// `tripwire` cannot make this pass by accident.
#[test]
fn macros_never_evaluate_their_arguments() {
    let before = EVALUATIONS.load(Ordering::SeqCst);

    // `scope!`, both arms, both implementations.
    profiling::scope!(tripwire(detonate()));
    profiling::scope!(tripwire(detonate()), tripwire(detonate()));
    crate::upstream_empty_impl::scope!(tripwire(detonate()));
    crate::upstream_empty_impl::scope!(tripwire(detonate()), tripwire(detonate()));

    // The parity macros. No consumer calls them, so this is their only
    // coverage — and the arm with an argument is the only one that could
    // evaluate anything.
    profiling::function_scope!();
    profiling::function_scope!(tripwire(detonate()));
    profiling::register_thread!();
    profiling::register_thread!(tripwire(detonate()));
    profiling::finish_frame!();
    crate::upstream_empty_impl::function_scope!();
    crate::upstream_empty_impl::function_scope!(tripwire(detonate()));
    crate::upstream_empty_impl::register_thread!();
    crate::upstream_empty_impl::register_thread!(tripwire(detonate()));
    crate::upstream_empty_impl::finish_frame!();

    assert_eq!(
        EVALUATIONS.load(Ordering::SeqCst),
        before,
        "a profiling macro evaluated its argument"
    );
}

/// Every verbatim consumer form compiles AND runs without touching anything.
///
/// The compile is the assertion the module bodies make; running them is what
/// catches an argument that is evaluated only on some path — and it is the
/// reason `instance.rs:469`'s `alloc::format!` is here rather than merely
/// type-checked.
#[test]
fn consumer_forms_run_without_evaluating_anything() {
    let before = EVALUATIONS.load(Ordering::SeqCst);

    wgpu_api_queue::forms();
    wgpu_hal_backends::forms();
    wgpu_core_two_arg::forms();

    assert_eq!(EVALUATIONS.load(Ordering::SeqCst), before);
}

/// The two borrowing call sites, run for real.
///
/// Compiling `wgpu_core_borrow_shape` is the borrowck assertion; this is the
/// behavioural one — `pass.parent` is still there to be taken, and the label
/// the macro "read" was never touched.
#[test]
fn borrow_shape_matches_upstream() {
    use wgpu_core_borrow_shape::{Base, ComputePass, Parent, RenderPass};

    let labelled = |label: Option<&str>| Base {
        label: label.map(str::to_owned),
    };

    let mut render = RenderPass {
        base: labelled(Some("aterm::frame")),
        parent: Some(Parent),
    };
    let (_parent, base) = wgpu_core_borrow_shape::render_pass_end(&mut render)
        .expect("the parent is still takeable after the macro");
    assert!(render.parent.is_none());
    // The label the macro appeared to read comes back whole: it was not
    // borrowed past the statement, moved, cloned or formatted.
    assert_eq!(base.label.as_deref(), Some("aterm::frame"));

    let mut compute = ComputePass {
        base: labelled(None),
        parent: Some(Parent),
    };
    let (_parent, base) = wgpu_core_borrow_shape::compute_pass_end(&mut compute)
        .expect("the parent is still takeable after the macro");
    assert!(compute.parent.is_none());
    // `unwrap_or("")` inside the macro never ran, so the `None` is still a
    // `None` rather than an empty string.
    assert_eq!(base.label, None);

    let mut upstream = RenderPass {
        base: labelled(Some("aterm::frame")),
        parent: Some(Parent),
    };
    let (_parent, base) = wgpu_core_borrow_shape::upstream_render_pass_end(&mut upstream)
        .expect("the parent is still takeable after the macro");
    assert_eq!(base.label.as_deref(), Some("aterm::frame"));
}

/// The `use profiling::scope;` spelling, which NO consumer writes.
///
/// All 198 sites use the absolute path, so this is insurance rather than a
/// copy — and it pins something the module docs claim: `#[macro_export]` puts
/// the macros at the crate ROOT, so both spellings resolve. The `deny` is the
/// assertion that a macro import is still *used* even though the macro expands
/// to nothing — name resolution runs before expansion, so the `use` is what
/// resolves the name. If that stopped being true this module would stop
/// compiling.
#[deny(unused_imports)]
mod imported_spelling {
    use profiling::{finish_frame, function_scope, register_thread, scope};

    pub fn forms() {
        scope!("aterm::insurance");
        scope!("aterm::insurance", "data");
        function_scope!();
        register_thread!("aterm-gpu");
        finish_frame!();
    }
}

#[test]
fn bare_macro_imports_resolve_and_stay_used() {
    imported_spelling::forms();
}
