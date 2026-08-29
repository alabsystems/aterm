// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ORACLE: every `log` invocation form in aterm's dependency graph, copied
//! verbatim from the consumer that writes it, plus a transcribed table oracle
//! for everything this crate computes.
//!
//! # Why this file exists
//!
//! Eight of the twenty crates this shim serves are absent from the mac cell
//! entirely — they are Windows-only, Linux-only, or a Windows build-dependency —
//! and a ninth is worse than absent: `wgpu-hal` DOES compile here, with exactly
//! the parts that use `log` hardest cfg'd out. It resolves on aarch64-apple-darwin
//! to features
//! `default, metal, portable-atomic` — no `dx12`, no `gles`, no `vulkan` — so a
//! local `cargo check` reaches naga's three `log!` sites and its one
//! `log_enabled!`, and nothing else. Invisible here:
//!
//! * all seven `wgpu-hal` `log!` sites and all four `max_level()` gates,
//! * the `const MESSAGE_PREFIXES: &[(&str, log::Level)]` array,
//! * `gpu-allocator`'s `use log::*;` glob and its two raw-string `log!`s,
//! * smithay-client-toolkit's eighteen `target:` forms — the one form that is a
//!   HARD COMPILE ERROR against a `format_args!`-splicing macro, and the reason
//!   this crate does not re-export `aterm_log`'s,
//! * `gl_generator`'s `#[macro_use] extern crate log;`, a Windows
//!   build-dependency.
//!
//! So "the build is green" proves almost nothing about this surface. The proof
//! is moved into a place that *does* compile everywhere. Every invocation below
//! is a byte-for-byte copy of a real call site, carrying a comment naming the
//! crate, file and line it came from; only the surrounding scaffolding — the
//! locals, structs and methods the arguments mention — is written here.
//! **Compiling this test is the proof that the surface accepts what the real
//! consumers write.** When a consumer bumps and introduces a form this shim
//! cannot parse, the fix is to paste the new line in here first and watch it
//! fail.
//!
//! A few invocations are marked `(synthetic)`. Those cover a name that a
//! verbatim `use` line imports but whose own call site lives in a different file
//! of the same crate; they exist so `#![deny(unused_imports)]` below has
//! something to bite on for every imported name.
//!
//! # The differential oracle, and why it is TRANSCRIBED rather than executed
//!
//! `crates/aterm-digest` keeps sha2/hmac, `crates/aterm-regex` keeps regex and
//! `crates/aterm-grapheme` keeps unicode-width as differential
//! `[dev-dependencies]`: the crate they replace, kept out of the shipped graph,
//! asserted equal. That pattern is UNAVAILABLE here, structurally, and the
//! measurement is in this crate's `Cargo.toml`: because this package is the
//! `[patch.crates-io]` target for the name `log`, a dev-dependency on crates.io
//! `log` — under any rename, since patch matches on package name — is redirected
//! back at this package. In a throwaway workspace that reproduces the
//! arrangement it resolves cleanly and then fails with
//! `error[E0433]: failed to resolve: use of unresolved module `upstream_log``.
//! A differential that survived would be comparing the shim to itself.
//!
//! What stands in its place is `upstream_table_oracle` and its neighbours: every
//! constant this crate reproduces is asserted against the value read out of
//! `log-0.4.32`'s own source, cited by file and line, so a reviewer can diff two
//! numbers instead of trusting a paraphrase. That is transcription checked by a
//! human, not co-execution, and calling it a differential would be a lie. The
//! only route to a live differential is a separate workspace that does not
//! inherit the root patch table — `tools/freeze-safety-gate` is the existing
//! precedent — which was judged disproportionate for a six-element lattice.
//!
//! # Two invariants this file holds, both re-derivable by hand
//!
//! 1. **Every distinct `use log::…` line in the twenty consumer trees appears
//!    here verbatim.** There are twelve of them. Re-derive with
//!    `grep -rhE "^\s*use log::" <each consumer>/src | sed 's/^ *//' | sort -u`
//!    and check each against this file. A module whose import line is
//!    *narrowed* to only the names it happens to call is no longer a copy of the
//!    consumer, and it quietly weakens the `deny(unused_imports)` assertion.
//! 2. **Every distinct argument SHAPE appears here.** Shape means the token
//!    classes, not the identifiers: `warn!(MSG, IDENT)` and `debug!(MSG, IDENT)`
//!    are one shape; `debug!(target: LIT, MSG, a, b, c)` is a different one; a
//!    raw string with doubled braces spanning ten lines is a third.
//!
//! Neither invariant is compiler-enforced. They are what a reviewer re-derives,
//! and they are why this file is worth more than the sum of its `assert!`s.
//!
//! # The forms that are NOT a bare `mac!(…);`
//!
//! It is easy to read a list of macro calls and conclude the grammar is the
//! whole test. It is not. Seven shapes here are about the *position* — the
//! statement or expression the call sits in — rather than the arguments:
//!
//! * **Match-arm, no trailing semicolon** — the macro IS the arm's value
//!   (smithay `seat/keyboard/mod.rs:566`, termwiz
//!   `escape/parser/mod.rs:209`). This is what breaks if the expansion is
//!   nothing at all rather than `{}`.
//! * **Closure tail position inside `map_err`** — wgpu-types `backend.rs:800`,
//!   where the expansion's type has to be the closure's return type.
//! * **Function tail position** — wgpu-core `command/ray_tracing.rs:918`,
//!   `log::debug!("only rebuild implemented")` with no semicolon at all.
//! * **`if EXPR` condition** — naga `proc/overloads/list.rs:98`,
//!   `if log::log_enabled!(log::Level::Debug) {`, which forces `log_enabled!` to
//!   be an expression of type `bool`.
//! * **`const` item position** — wgpu-hal `auxil/dxgi/exception.rs:30`,
//!   `const MESSAGE_PREFIXES: &[(&str, log::Level)]`, which forces `Level` to be
//!   const-constructible.
//! * **Inside another `macro_rules!` transcriber** — wgpu-core
//!   `lib.rs:171-200`. Its `api_log!` / `api_log_debug!` / `resource_log!`
//!   wrappers forward `$($arg:tt)+` to `log::trace!` / `log::debug!` /
//!   `log::info!`, and 150 of wgpu-core's 203 logging sites reach this shim
//!   only through them. A census that greps `log::` sees 53 and stops.
//! * **Under an attribute on the macro STATEMENT** — wgpu-hal
//!   `vulkan/swapchain/native.rs:596`, `#[cfg(not(target_os = "android"))]`
//!   directly in front of a `log::debug!`. The graph's only one, and the only
//!   place `cfg`-stripping and macro expansion meet.
//!
//! # The runtime half
//!
//! Accepting the syntax is only half the contract. The other half is that the
//! macros evaluate **nothing** and that `max_level()` is `Off`. Both are pinned
//! by tests that ARM THEIR TRIPWIRES FIRST — `tripwires_are_armed` proves the
//! counter moves and the panic fires on a direct call, and
//! `max_level_is_off_from_this_crates_own_static` proves `>=` is a live
//! comparison by exhibiting the one case that is true. An unarmed tripwire makes
//! every later assertion vacuous.
//!
//! # What the lint attributes below are evidence of
//!
//! `#![deny(unused_imports)]` is a load-bearing assertion, not hygiene: it proves
//! that `use log::warn;` is still *used* even though `warn!` expands to nothing.
//! Name resolution runs before expansion, so the import is what resolves the
//! macro. If that changed, this file would stop compiling — which is the point.
//!
//! The `allow`s are the mirror image: they are the documented divergence,
//! demonstrated. Discarding the token trees leaves the values the arguments
//! mention genuinely unmentioned, so `unused_variables` (a local read only
//! inside a macro argument) and `dead_code` (a method called only from one) both
//! fire here. Every real file affected is a registry crate compiled with
//! `--cap-lints allow`, or the registry build-dep `gl_generator`; no first-party
//! aterm crate depends on crates.io `log`. See the shim's module docs for why
//! `let _ = &$arg;` is not an acceptable fix.

#![deny(unused_imports)]
#![allow(unused_variables, dead_code)]

// gl_generator-0.14.0/lib.rs:63-64 — the graph's ONLY `#[macro_use] extern
// crate` consumer, an edition-2015 Windows build-dependency of glutin_wgl_sys.
// Reproduced at this crate root because that is the only place the form exists:
// it puts the macros in the macro_use prelude rather than in a module's scope,
// and a `#[macro_export]` macro is what has to be visible to it.
//
// It does NOT weaken the `deny(unused_imports)` assertion below. An explicit
// `use log::debug;` in a module takes priority over the macro_use prelude, so
// each module's verbatim import is still what resolves its macros — if that
// stopped being true, those imports would go unused and this file would stop
// compiling.
#[macro_use]
extern crate log;

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

// ===========================================================================
// Tripwires — the runtime half of the contract
// ===========================================================================

/// Bumped once per real evaluation of [`control_tripwire`]. Read only by
/// `tripwires_are_armed`, which is why it is a second counter rather than a
/// shared one: `cargo test` runs the two tests concurrently, and one counter
/// would make the arming test race the assertion it is arming.
static CONTROL_EVALUATIONS: AtomicUsize = AtomicUsize::new(0);

/// Bumped once per real evaluation of [`tripwire`].
static MACRO_EVALUATIONS: AtomicUsize = AtomicUsize::new(0);

/// The CONTROL for the counter tripwire: identical to [`tripwire`], called
/// directly, so `tripwires_are_armed` can prove the detector fires.
fn control_tripwire<T>(value: T) -> T {
    CONTROL_EVALUATIONS.fetch_add(1, Ordering::SeqCst);
    value
}

/// An identity function with an observable side effect, placed in macro
/// argument positions. If the shim ever evaluated its arguments, the counter
/// would move.
fn tripwire<T>(value: T) -> T {
    MACRO_EVALUATIONS.fetch_add(1, Ordering::SeqCst);
    value
}

/// An argument that destroys the test if it is ever evaluated.
///
/// The counter alone would catch an accidental evaluation; this catches it
/// louder, and covers the `let _ = &$value;` shape specifically — taking a
/// reference to `detonate()` still calls it.
fn detonate() -> &'static str {
    panic!("a log macro evaluated its argument");
}

// ===========================================================================
// Scaffolding shared by the consumer modules
// ===========================================================================

/// Stands in for `gpu_allocator::result`, whose glob import collides with
/// `use log::*;` three lines above it in the real file.
///
/// The names are the real ones (`gpu-allocator-0.28.0/src/result.rs:6,27`).
/// Two globs supplying the same name is an error only AT THE USE SITE, so this
/// module is the only way to test the hazard from macOS at all.
pub mod result {
    #[derive(Debug)]
    pub enum AllocationError {
        OutOfMemory,
    }
    pub type Result<V, E = AllocationError> = ::core::result::Result<V, E>;
}

// ===========================================================================
// wgpu-hal 29.0.3 — the crate this shim exists for
// ===========================================================================

// THE `rustfmt::skip` ON EVERY CONSUMER MODULE IS LOAD-BEARING.
//
// The invocations are byte-for-byte copies of third-party source, and their
// exact shape — where the line breaks fall, which trailing commas are present,
// how a raw string is indented — is part of what is being tested. rustfmt would
// happily rewrite a three-line `log!` into one line that wgpu-hal does not
// contain, at which point the file would still compile and would no longer be
// evidence of anything.

#[rustfmt::skip]
mod wgpu_hal_dxgi_exception {
    // wgpu-hal-29.0.3/src/auxil/dxgi/exception.rs — Windows only, and the only
    // place in the graph that puts `log::Level` in a `const` item.
    use std::borrow::Cow;

    // wgpu-hal-29.0.3/src/auxil/dxgi/exception.rs:30-38
    const MESSAGE_PREFIXES: &[(&str, log::Level)] = &[
        ("CORRUPTION", log::Level::Error),
        ("ERROR", log::Level::Error),
        ("WARNING", log::Level::Warn),
        // We intentionally suppress "INFO" messages down to debug
        // so that users are not innundated with info messages from the runtime.
        ("INFO", log::Level::Debug),
        ("MESSAGE", log::Level::Trace),
    ];

    pub fn forms() -> i32 {
        let message: Cow<'_, str> = Cow::Borrowed("WARNING: something #82 happened");
        let message: &str = &message;

        // wgpu-hal-29.0.3/src/auxil/dxgi/exception.rs:65-71 — `Level` read out
        // of a `const` slice by pattern, then used twice. This is what makes
        // `Copy` and `PartialEq` load-bearing derives rather than decoration.
        let (message, level) = match MESSAGE_PREFIXES
            .iter()
            .find(|&&(prefix, _)| message.starts_with(prefix))
        {
            Some(&(prefix, level)) => (&message[prefix.len() + 2..], level),
            None => (message, log::Level::Debug),
        };

        // wgpu-hal-29.0.3/src/auxil/dxgi/exception.rs:73
        if level == log::Level::Warn && message.contains("#82") {
            return 0;
        }

        // wgpu-hal-29.0.3/src/auxil/dxgi/exception.rs:84-86 — a `log!` inside a
        // `catch_unwind` closure on the D3D12 debug-callback thread. Today the
        // closure body is empty; see "THE SEAM" in the shim for why that matters
        // to a future `forward` decision.
        let _ = std::panic::catch_unwind(|| {
            log::log!(level, "{message}");
        });

        // wgpu-hal-29.0.3/src/auxil/dxgi/exception.rs:89
        if cfg!(debug_assertions) && level == log::Level::Error {
            return 1;
        }
        0
    }
}

#[rustfmt::skip]
mod wgpu_hal_gles_egl {
    // wgpu-hal-29.0.3/src/gles/egl.rs — Linux/Android only.
    pub fn forms() -> usize {
        let log_severity = log::Level::Warn;
        let command = "eglInitialize";
        let error = 0x3001u32;
        let message = "not initialized";

        // wgpu-hal-29.0.3/src/gles/egl.rs:78 — inline captures AND a trailing
        // comma after the last argument, inside the argument list.
        log::log!(log_severity, "EGL '{command}' code 0x{error:x}: {message}",);

        // wgpu-hal-29.0.3/src/gles/egl.rs:424 — `max_level()` gating REAL WORK:
        // the body calls `egl.get_config_count()` and `egl.get_configs()`.
        let mut config_count = 0usize;
        if log::max_level() >= log::LevelFilter::Trace {
            log::trace!("Configurations:");
            config_count = 64;
        }
        config_count
    }
}

#[rustfmt::skip]
mod wgpu_hal_gles_mod {
    pub fn forms() {
        let log_severity = log::Level::Debug;
        let source_str = "API";
        let type_str = "Error";
        let id = 7u32;
        let message = "GL_INVALID_ENUM";

        // wgpu-hal-29.0.3/src/gles/mod.rs:1105-1109 — multi-line `log!` with a
        // single all-inline-capture format string and NO trailing comma.
        let _ = std::panic::catch_unwind(|| {
            log::log!(
                log_severity,
                "GLES: [{source_str}/{type_str}] ID {id} : {message}"
            );
        });
    }
}

#[rustfmt::skip]
mod wgpu_hal_vulkan_instance {
    // wgpu-hal-29.0.3/src/vulkan/instance.rs — the four most consequential
    // lines in this whole extraction.
    pub fn forms() -> u32 {
        let level = log::Level::Warn;
        let message_type = "VALIDATION";
        let message_id_name = "VUID-vkCreateDevice";
        let message_id_number = -1_234_567_890i32;
        let message = "the message";
        // wgpu-hal builds this with `.flat_map(…).collect::<Vec<_>>()`; the
        // shape matters because `names.join(", ")` is the macro argument that
        // must not be evaluated.
        let names = ["a", "b"].iter().map(|s| String::from(*s)).collect::<Vec<_>>();

        // wgpu-hal-29.0.3/src/vulkan/instance.rs:97-105 — multi-line `log!`,
        // positional arguments, WITH a trailing comma, inside `catch_unwind`.
        let _ = std::panic::catch_unwind(|| {
            log::log!(
                level,
                "{:?} [{} (0x{:x})]\n\t{}",
                message_type,
                message_id_name,
                message_id_number,
                message,
            );
        });

        // wgpu-hal-29.0.3/src/vulkan/instance.rs:117, :130, :150 — same shape,
        // one line, argument is a method call that allocates.
        let _ = std::panic::catch_unwind(|| {
            log::log!(level, "\tqueues: {}", names.join(", "));
        });

        // wgpu-hal-29.0.3/src/vulkan/instance.rs:717-726 — THE BITMASK. These
        // three comparisons decide which severities the Vulkan driver is asked
        // to report. All three are false today; `max_level_is_off_from_this_\
        // crates_own_static` is the test that keeps them that way.
        let mut severity = 0x1u32; // vk::…::ERROR, unconditional
        if log::max_level() >= log::LevelFilter::Debug {
            severity |= 0x2; // …::VERBOSE
        }
        if log::max_level() >= log::LevelFilter::Info {
            severity |= 0x4; // …::INFO
        }
        if log::max_level() >= log::LevelFilter::Warn {
            severity |= 0x8; // …::WARNING
        }
        severity
    }
}

#[rustfmt::skip]
mod wgpu_hal_dx12_command {
    enum RootElement { Empty }

    pub fn forms() {
        let index = 3usize;
        let element = RootElement::Empty;

        // wgpu-hal-29.0.3/src/dx12/command.rs:203 — MATCH ARM, expression
        // position, no trailing semicolon. The arm's value IS the macro.
        match element {
            RootElement::Empty => log::error!("Root index {index} is not bound"),
        }
    }
}

// ===========================================================================
// naga 29.0.3 — the only consumer a macOS `cargo check` actually exercises
// ===========================================================================

#[rustfmt::skip]
mod naga_forms {
    // naga-29.0.3/src/diagnostic_filter.rs:41 — `log::Level` as a bound in a
    // closure PARAMETER TYPE. The shim's `Level` has to be nameable in a
    // signature, not just constructible.
    fn report_diag<E>(err: E, log_handler: impl FnOnce(E, log::Level)) -> Result<(), E> {
        log_handler(err, log::Level::Warn);
        Ok(())
    }

    pub fn forms() {
        // naga-29.0.3/src/front/spv/mod.rs:845-853 — a `Level` chosen by a
        // `match`, then passed by value into `log!`.
        let other = "Block";
        let level = match other {
            "Block" => log::Level::Debug,
            _ => log::Level::Warn,
        };
        log::log!(level, "Unknown decoration {other:?}");

        // naga-29.0.3/src/valid/analyzer.rs:912 — a CLOSURE WHOSE ENTIRE BODY
        // is the macro, passed as an argument.
        let _ = report_diag("boom", |e, level| log::log!(level, "{e}"));

        // naga-29.0.3/src/front/wgsl/parse/directive.rs:45-48 — a multi-line
        // closure whose tail is the macro, with a method call as the argument.
        let source = "@diagnostic(off, derivative_uniformity)";
        let _ = report_diag("boom", |e, level| {
            let e = e.len();
            log::log!(level, "{}", emit_to_string(e, source));
        });

        // naga-29.0.3/src/proc/overloads/list.rs:96-99 — `log_enabled!` in `if`
        // CONDITION position. This is what forces the macro to be a `bool`
        // expression rather than a statement.
        let i = 0usize;
        if log::log_enabled!(log::Level::Debug) {
            log::debug!("    considering rule {:?}", i);
        }
    }

    fn emit_to_string(e: usize, source: &str) -> String {
        format!("{e}: {source}")
    }
}

#[rustfmt::skip]
mod wgpu_core_and_types {
    pub fn ray_tracing() {
        // wgpu-core-29.0.3/src/command/ray_tracing.rs:918 — FUNCTION TAIL
        // position, no semicolon anywhere. The expansion must be an expression
        // of the function's return type, `()`.
        log::debug!("only rebuild implemented")
    }

    pub fn from_env(env: &str) -> Option<u8> {
        // wgpu-types-29.0.3/src/backend.rs:800-804 — the macro is the whole body
        // of a `map_err` closure, so its type is the closure's return type, and
        // the result is `.ok()`-ed. Inline capture of `env` plus a named local.
        env.parse::<u8>().map_err(|expected_msg| {
            log::warn!(
                "Unknown value `{env:?}` for `WGPU_DX12_COMPILER` environment variable. {expected_msg}"
            )
        })
        .ok()
    }
}

// ===========================================================================
// wgpu-core 29.0.3 — the WRAPPER MACROS: 150 call sites that reach `log`
// without naming it
// ===========================================================================

/// `wgpu-core-29.0.3/src/lib.rs:171-200`, verbatim — three `macro_rules!` that
/// re-emit a `log` macro, each declared twice behind a `#[cfg(feature = …)]`
/// pair.
///
/// THIS IS THE SHAPE AN ARGUMENT CENSUS CANNOT SEE. Grepping `log::` in
/// wgpu-core finds 53 direct sites; the crate logs from 203, because
/// `api_log!` (115 uses), `resource_log!` (33) and `api_log_debug!` (2) forward
/// to `log::trace!` / `log::debug!` / `log::info!`. Re-derive both halves with
///
/// ```text
/// grep -rhoE '\b(api_log|api_log_debug|resource_log)!' \
///     <registry>/wgpu-core-29.0.3 --include='*.rs' | sort | uniq -c
/// ```
///
/// (150 after subtracting the six definition lines and the three
/// `pub(crate) use`s), and it is the largest single block of `log` traffic in
/// aterm's graph.
///
/// Three properties this shape has and no direct call site here has:
///
/// * **The arguments arrive as an opaque `tt` REPETITION**, substituted out of
///   another macro's metavariable rather than written as literal tokens at the
///   call. The shim's `$($discarded:tt)*` accepts that; a matcher written as a
///   grammar (`$fmt:literal $(, $arg:expr)*`) would accept these particular
///   calls too, which is exactly why the form is worth *compiling* rather than
///   reasoning about — the failure would be a future bump, not today.
/// * **The transcriber is PARENTHESISED and ends without a semicolon**, so the
///   shim's `{}` lands directly in whatever position the `api_log!` CALL sits
///   in. 146 of the 150 are statements; the other four are the tail expression
///   of a match-arm block (`src/command/mod.rs:1080,1083,1099,1102`), and both
///   are reproduced below.
/// * **Both `#[cfg]` halves are written out.** The pair is how wgpu-core picks
///   `info!` over `trace!`/`debug!`. Neither feature is enabled in aterm's
///   graph, and neither is declared by this test crate, so the `not(…)` arm is
///   the live one here exactly as it is there — which is also why
///   `unexpected_cfgs` has to be allowed on this module and nowhere else in the
///   file: editing the predicate out would destroy the thing being copied.
#[rustfmt::skip]
#[allow(unexpected_cfgs)]
mod wgpu_core_wrapper_macros {
    // wgpu-core-29.0.3/src/lib.rs:171-190, verbatim.
    #[cfg(feature = "api_log_info")]
    macro_rules! api_log {
        ($($arg:tt)+) => (log::info!($($arg)+))
    }
    #[cfg(not(feature = "api_log_info"))]
    macro_rules! api_log {
        ($($arg:tt)+) => (log::trace!($($arg)+))
    }

    #[cfg(feature = "api_log_info")]
    macro_rules! api_log_debug {
        ($($arg:tt)+) => (log::info!($($arg)+))
    }
    #[cfg(not(feature = "api_log_info"))]
    macro_rules! api_log_debug {
        ($($arg:tt)+) => (log::debug!($($arg)+))
    }

    pub(crate) use api_log;
    pub(crate) use api_log_debug;

    // wgpu-core-29.0.3/src/lib.rs:192-200, verbatim.
    #[cfg(feature = "resource_log_info")]
    macro_rules! resource_log {
        ($($arg:tt)+) => (log::info!($($arg)+))
    }
    #[cfg(not(feature = "resource_log_info"))]
    macro_rules! resource_log {
        ($($arg:tt)+) => (log::trace!($($arg)+))
    }
    pub(crate) use resource_log;
}

/// The call sites, reached the way wgpu-core reaches them: `use crate::{…}` on
/// the wrapper names (`src/instance.rs:15`, `src/resource.rs:7`,
/// `src/device/resource.rs:103`), spelled `super::` here because this file is
/// one crate rather than one crate root.
#[rustfmt::skip]
mod wgpu_core_wrapper_sites {
    use super::wgpu_core_wrapper_macros::{api_log, api_log_debug, resource_log};

    pub struct Desc { pub label: Option<String>, pub mapped_at_creation: bool }

    pub fn statement_forms(desc: &Desc, id: u32) {
        // wgpu-core-29.0.3/src/instance.rs:459 — bare literal through the
        // wrapper.
        api_log!("Instance::enumerate_adapters");

        // wgpu-core-29.0.3/src/instance.rs:474 — one positional argument.
        let adapter_info = "Adapter { backend: Metal }";
        api_log_debug!("Adapter {:?}", adapter_info);

        // wgpu-core-29.0.3/src/resource.rs:1237 — the `resource_log!` wrapper.
        resource_log!("Destroy raw StagingBuffer");

        // wgpu-core-29.0.3/src/device/global.rs:133-141 — MULTI-LINE through
        // the wrapper, with an inline `{id:?}` capture, a method-call argument
        // and an `if`-expression argument. Every one of those tokens crosses
        // two macro boundaries before reaching the shim.
        api_log!(
            "Device::create_buffer({:?}{}) -> {id:?}",
            desc.label.as_deref().unwrap_or(""),
            if desc.mapped_at_creation {
                ", mapped_at_creation"
            } else {
                ""
            }
        );
    }

    /// wgpu-core-29.0.3/src/command/mod.rs:1078-1085 — the four sites where a
    /// wrapper call is the TAIL EXPRESSION of a match-arm block rather than a
    /// statement. The arm's value is whatever `log::trace!` expands to, so the
    /// `match` types as `()` only because the expansion is an expression.
    ///
    /// THE `allow` IS EVIDENCE, not hygiene, and it is a divergence this file
    /// had not recorded before. Because the shim expands to `{}`, clippy sees
    /// the `Ok(_)` arm as an empty block and fires `single_match`, offering to
    /// rewrite wgpu-core's two-arm `match` as an `if let`. Against the real
    /// facade the arm contains a live `if lvl <= max_level()` and the lint does
    /// not fire. It costs nothing in the shipped graph — wgpu-core is a
    /// registry crate and gets `--cap-lints allow` — and it costs an `allow`
    /// here, where this workspace's `-D warnings` gate does apply. It belongs
    /// with the `unused_variables` / `dead_code` divergence the file header
    /// already documents: the same cause, a different lint.
    #[allow(clippy::single_match)]
    pub fn match_arm_tail(res: &Result<(), String>) {
        match res.as_ref() {
            Err(err) => {
                api_log!("Finished encoding render pass ({err:?})")
            }
            Ok(_) => {
                api_log!("Finished encoding render pass (success)")
            }
        }
    }

    /// The tripwire form, so this module is covered by the
    /// "evaluates nothing" test as well as by the compiler. Forwarding through
    /// a `tt` repetition must not introduce an evaluation the direct sites
    /// would not have had.
    pub fn never_evaluates() {
        api_log!("{}", super::tripwire(super::detonate()));
        api_log_debug!("{}", super::tripwire(super::detonate()));
        resource_log!("{}", super::tripwire(super::detonate()));
    }
}

// ===========================================================================
// wgpu-hal 29.0.3 — the graph's ONLY ATTRIBUTED log macro statement
// ===========================================================================

/// `wgpu-hal-29.0.3/src/vulkan/swapchain/native.rs:596-597`, verbatim.
///
/// A `#[cfg]` on the macro invocation ITSELF, which is a different thing from
/// every other site in this file: the attribute has to attach to a *macro
/// invocation statement* and survive the expansion. It is the only one in the
/// graph — re-derive with
///
/// ```text
/// grep -rzoP '#\[[^\]]*\]\s*\n\s*(log::)?(error|warn|info|debug|trace|log)!' \
///     <each of the twenty consumer trees>
/// ```
///
/// which returns this site and nothing else. `not(target_os = "android")` is
/// TRUE on all four of aterm's cells, so the statement is live here rather than
/// cfg'd away — which is what makes compiling this module evidence.
#[rustfmt::skip]
mod wgpu_hal_vulkan_swapchain {
    pub struct Texture { pub index: u32 }

    pub fn present(texture: &Texture, suboptimal: bool) {
        if suboptimal {
            // We treat `VK_SUBOPTIMAL_KHR` as `VK_SUCCESS` on Android.
            // On Android 10+, libvulkan's `vkQueuePresentKHR` implementation returns `VK_SUBOPTIMAL_KHR` if not doing pre-rotation
            // (i.e `VkSwapchainCreateInfoKHR::preTransform` not being equal to the current device orientation).
            // This is always the case when the device orientation is anything other than the identity one, as we unconditionally use `VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR`.
            #[cfg(not(target_os = "android"))]
            log::debug!("Suboptimal present of frame {}", texture.index);
        }
    }

    /// (synthetic) THE CONTROL FOR THE SITE ABOVE, plus the stacked form.
    ///
    /// The verbatim site's predicate is true on all four cells, so on its own
    /// it proves only that an attribute may SIT in front of a macro statement —
    /// not that the attribute is being APPLIED to it. Neither a runtime control
    /// nor an undefined-name-in-an-ARGUMENT control closes that gap, and the
    /// second was tried and MEASURED before this one was written:
    /// `#[cfg(any())] log::debug!("{}", never_compiled());` compiles green with
    /// the attribute DELETED, because the shim discards its token tree and
    /// `never_compiled` is therefore never resolved. An undefined name inside a
    /// no-op macro's arguments detects nothing at all.
    ///
    /// What does detect it is the macro NAME, which is resolved before
    /// expansion. `log::no_such_macro!` does not exist, so if the attribute
    /// were being ignored this module would fail with ``cannot find macro
    /// `no_such_macro` in `log` ``. It compiles; therefore the attribute really
    /// did remove the statement. Verified in both directions — deleting the
    /// `#[cfg(any())]` line produces exactly that error.
    ///
    /// The second is the stacked form — two `#[cfg]`s on one macro statement,
    /// which nothing in the graph writes today.
    ///
    /// WHAT IS DELIBERATELY *NOT* HERE, because it was measured and it is not a
    /// shape: a lint attribute on a macro invocation statement. Writing
    /// `#[allow(unused_variables)] log::warn!(…)` compiles, but rustc answers
    /// `warning: unused attribute` — "the built-in attribute `allow` will be
    /// ignored, since it's applied to the macro invocation `log::warn`". Only
    /// `cfg` and `cfg_attr` are honoured in that position, so there is no
    /// stacked-lint-attribute case for this shim to get wrong, and a test
    /// carrying one would fail this workspace's `-D warnings` gate rather than
    /// prove anything.
    pub fn attribute_controls() {
        #[cfg(any())]
        log::no_such_macro!("the attribute is what keeps this name unresolved");

        #[cfg(not(target_os = "android"))]
        #[cfg(not(target_os = "ios"))]
        log::warn!("stacked cfgs on one macro statement");
    }
}

// ===========================================================================
// gpu-allocator 0.28.0 — windows + linux. The glob-collision hazard.
// ===========================================================================

#[rustfmt::skip]
mod gpu_allocator_allocator_mod {
    // gpu-allocator-0.28.0/src/allocator/mod.rs:8 and :10 — TWO GLOB IMPORTS,
    // two lines apart. `crate::result` exports `AllocationError` and `Result`,
    // so any name this shim exported that also lives there would be an error
    // right here, in a file that only builds on Windows and Linux. That is why
    // the shim's exported name set is a strict subset of upstream `log`'s.
    use log::*;

    use crate::result::*;

    pub trait SubAllocator {
        // gpu-allocator-0.28.0/src/allocator/mod.rs:132-137 — `Level` (reached
        // THROUGH THE GLOB, unqualified) as a trait-method parameter type, and
        // `Result` reached through the other glob in the same signature.
        fn report_memory_leaks(&self, log_level: Level, memory_type_index: usize);
        fn free(&mut self, chunk_id: u64) -> Result<()>;
    }

    pub struct Dummy;

    impl SubAllocator for Dummy {
        fn report_memory_leaks(&self, log_level: Level, memory_type_index: usize) {
            // (synthetic) `debug!` and `warn!` ride the same glob; gpu-allocator
            // writes 16 `debug!` and 2 `warn!` through it.
            debug!("reporting leaks for memory type {memory_type_index}");
            warn!("leaked at {log_level}");
        }

        fn free(&mut self, chunk_id: u64) -> Result<()> {
            if chunk_id == 0 {
                return Err(AllocationError::OutOfMemory);
            }
            Ok(())
        }
    }

    pub fn forms() {
        let mut d = Dummy;
        d.report_memory_leaks(Level::Debug, 0);
        let _ = d.free(1);
    }
}

#[rustfmt::skip]
mod gpu_allocator_free_list {
    // gpu-allocator-0.28.0/src/allocator/free_list_allocator/mod.rs:16
    // (the same line is at dedicated_block_allocator/mod.rs:12).
    use log::{log, Level};

    pub fn forms() {
        let log_level = Level::Warn;
        let memory_type_index = 1usize;
        let memory_block_index = 2usize;
        let chunk_id = 3u64;
        let chunk_size = 0x1000u64;
        let chunk_offset = 0x40u64;
        let allocation_type = "Linear";
        let name = "buffer";
        let backtrace_info = "";

        // gpu-allocator-0.28.0/src/allocator/free_list_allocator/mod.rs:395-411
        // A RAW STRING spanning eleven lines, containing doubled braces (`{{`,
        // `}}`), positional holes AND an inline capture (`{backtrace_info}`),
        // with a trailing comma. Nothing else in the graph is shaped like this,
        // and a macro that parsed a grammar rather than tokens would have to get
        // every one of those right.
        log!(
            log_level,
            r#"leak detected: {{
    memory type: {}
    memory block: {}
    chunk: {{
        chunk_id: {},
        size: 0x{:x},
        offset: 0x{:x},
        allocation_type: {:?},
        name: {}{backtrace_info}
    }}
}}"#,
            memory_type_index,
            memory_block_index,
            chunk_id,
            chunk_size,
            chunk_offset,
            allocation_type,
            name,
        );

        // gpu-allocator-0.28.0/src/vulkan/mod.rs:921 — `Level` (imported bare)
        // as a public method parameter.
        report_memory_leaks(log_level);
    }

    fn report_memory_leaks(log_level: Level) {
        let _ = log_level;
    }
}

// ===========================================================================
// smithay-client-toolkit 0.19.2 — LINUX ONLY, and the whole `target:` obligation
// ===========================================================================
//
// All eighteen `target:` invocations in the graph are in this crate: 9 `warn!`,
// 5 `debug!`, 3 `error!`, 1 `trace!`. They are the reason the shim writes its
// own macros instead of re-exporting `aterm_log`'s, which splice
// `$($arg:tt)*` straight into `format_args!` and therefore reject the colon with
// `error: expected `,`, found `:``. Every distinct shape is reproduced below.

#[rustfmt::skip]
mod smithay_client_toolkit {
    // `single_match` is right in general and wrong here: the `match` is the
    // point. smithay writes the macro as an ARM VALUE with no trailing
    // semicolon, which is the form that would break if the expansion were
    // nothing at all rather than `{}`. Collapsing it to `if` deletes the test.
    #[allow(clippy::single_match)]
    pub fn forms() {
        struct Global { name: u32 }
        struct Iface { name: &'static str, version: u32 }
        let global = Global { name: 4 };
        let iface = Iface { name: "wl_shm", version: 1 };
        let version = 1u32;
        let range = 1u32..=3u32;

        // smithay-client-toolkit-0.19.2/src/registry.rs:249
        log::debug!(target: "sctk", "Bound new global [{}] {} v{}", global.name, iface.name, version);

        // smithay-client-toolkit-0.19.2/src/registry.rs:485
        log::trace!(target: "sctk", "Version {} of {} is available; binding is currently limited to {}", iface.version, iface.name, range.end());

        let format = "Argb8888";
        // smithay-client-toolkit-0.19.2/src/shm/mod.rs:123
        log::debug!(target: "sctk", "supported wl_shm format {:?}", format);

        let raw = 0x34325241u32;
        // smithay-client-toolkit-0.19.2/src/shm/mod.rs:128
        log::debug!(target: "sctk", "Unknown supported wl_shm format {:x}", raw);

        let unknown = 0x2u32;
        // smithay-client-toolkit-0.19.2/src/shell/xdg/window/inner.rs:225
        log::error!(target: "sctk", "unknown decoration mode 0x{:x}", unknown);

        let pointer = PointerId;
        // smithay-client-toolkit-0.19.2/src/seat/pointer/mod.rs:272 — the
        // argument is a METHOD CALL, which a discarding macro must not run.
        log::warn!(target: "sctk", "{}: invalid pointer button state: {:x}", pointer.id(), unknown);

        // smithay-client-toolkit-0.19.2/src/seat/keyboard/mod.rs:514 — bare
        // message, no arguments at all after the target.
        log::warn!(target: "sctk", "non-xkb compatible keymap");

        // smithay-client-toolkit-0.19.2/src/seat/keyboard/mod.rs:553
        log::error!(target: "sctk", "invalid keymap");

        let err = "bad";
        // smithay-client-toolkit-0.19.2/src/seat/keyboard/mod.rs:557
        log::error!(target: "sctk", "{}", err);

        let value = 0x2u32;
        // smithay-client-toolkit-0.19.2/src/seat/keyboard/mod.rs:566 — a
        // `target:` form in MATCH-ARM position with NO trailing semicolon.
        match value {
            0 => {},
            _ => log::warn!(target: "sctk", "unknown keymap format 0x{:x}", value)
        }
    }

    struct PointerId;
    impl PointerId {
        fn id(&self) -> u32 { 9 }
    }
}

// ===========================================================================
// The remaining Linux-only consumers, and the Windows build-dependency
// ===========================================================================

#[rustfmt::skip]
mod sctk_adwaita_buttons {
    // sctk-adwaita-0.10.1/src/buttons.rs:1
    use log::{debug, warn};

    pub fn forms() {
        for button in ["appmenu", "close", "wat"] {
            let _kind = match button {
                "close" => 0u8,
                // sctk-adwaita-0.10.1/src/buttons.rs:168 — an ESCAPED QUOTE
                // inside the format string.
                "appmenu" => {
                    debug!("Ignoring \"appmenu\" button");
                    continue;
                }
                _ => {
                    // sctk-adwaita-0.10.1/src/buttons.rs:172
                    warn!("Ignoring unknown button type: {button}");
                    continue;
                }
            };
        }
    }
}

#[rustfmt::skip]
mod calloop_loop_logic {
    // calloop-0.13.0/src/loop_logic.rs:17 (the crate's own import line)
    use log::trace;

    pub fn forms() {
        struct Token;
        impl Token { fn get_id(&self) -> u32 { 1 } }
        let entry_token = Token;

        // calloop-0.13.0/src/loop_logic.rs:198-201 — multi-line, one argument
        // which is a method call, no trailing comma.
        trace!(
            "[calloop] Updating registration of source #{}",
            entry_token.get_id()
        );
    }
}

#[rustfmt::skip]
mod tiny_skia_painter {
    pub fn forms() {
        let width = -1.0f32;
        if width < 0.0 {
            // tiny-skia-0.11.4/src/painter.rs:336 — the simplest possible form,
            // fully qualified, bare literal.
            log::warn!("negative stroke width isn't allowed");
        }
    }
}

#[rustfmt::skip]
mod wayland_sys_client {
    pub fn forms() {
        let ver = "libwayland-client.so.0";
        let s = "wl_display_connect";

        // wayland-sys-0.31.11/src/client.rs:101 — TWO inline captures, no
        // positional arguments.
        log::error!("Found library {ver} cannot be used: symbol {s} is missing.");
    }
}

#[rustfmt::skip]
mod xkbcommon_dl {
    // xkbcommon-dl-0.4.2/src/lib.rs:15 (the crate's own import line)
    use log::info;

    pub fn forms() {
        let module: Option<&str> = Some("xkbcommon");
        let name = "libxkbcommon.so.0";
        let e = "not found";

        // xkbcommon-dl-0.4.2/src/lib.rs:347-350 and :352 — both arms are
        // EXPRESSION position with no trailing semicolon, inside an if/else
        // that is itself a match arm's body.
        if let Some(module) = module {
            info!(
                "Failed loading {} module from `{}`. Error: {:?}",
                module, name, e
            )
        } else {
            info!("Failed loading `{}`. Error: {:?}", name, e)
        }
    }
}

#[rustfmt::skip]
mod gl_generator_registry_parse {
    // gl_generator-0.14.0 has NO `use log::…` line anywhere: every macro
    // reaches it through the crate-root `#[macro_use] extern crate log;` at the
    // top of this file. That is the form under test here.
    pub fn forms() {
        let one = "enum";
        let two = "unused";
        let end = "/require";

        // gl_generator-0.14.0/registry/parse.rs:475
        debug!("consume_two: looking for {} and {} until {}", one, two, end);

        // (synthetic) `warn!` also reaches gl_generator through the same
        // `#[macro_use]`; it writes 12 `debug!` and 2 `warn!` in total.
        warn!("unknown enum type");
    }
}

// ===========================================================================
// The five DEV-DEPENDENCY consumers, reached through aterm-bench / aterm-render
// ===========================================================================
//
// A `cargo tree -e normal` census does not see any of these, and they carry 147
// macro sites — 92 of them `trace!`, 61 in alacritty_terminal alone — plus the
// other match-arm form. Where these crates serve as aterm's differential
// ORACLES, blanking their diagnostics changes what a failing oracle run prints.

#[rustfmt::skip]
mod termwiz_parser {
    // termwiz-0.23.3/src/escape/parser/mod.rs:7
    use log::error;

    // See the note in `smithay_client_toolkit`: the `match` is what is under
    // test, so `single_match` must not be allowed to rewrite it.
    #[allow(clippy::single_match)]
    pub fn forms() {
        let byte = 0x9Bu8;
        let code: Option<u8> = None;

        // termwiz-0.23.3/src/escape/parser/mod.rs:209-212 — MATCH ARM,
        // multi-line, expression position, `byte as char` as an argument.
        match code {
            Some(code) => {},
            None => error!(
                "impossible C0/C1 control code {:?} 0x{:x} was dropped",
                byte as char, byte
            ),
        }
    }
}

#[rustfmt::skip]
mod vte_ansi {
    // vte-0.15.0/src/ansi.rs:29 — `log` is an OPTIONAL dependency of vte,
    // enabled in this graph. The same import line is also
    // gpu-allocator-0.28.0/src/metal/mod.rs:7.
    use log::debug;

    pub fn forms() {
        #[derive(Debug)]
        struct Rgb { r: u8, g: u8, b: u8 }
        let this = Rgb { r: 1, g: 2, b: 3 };
        let result = Rgb { r: 2, g: 4, b: 6 };
        let rhs = 2.0f32;

        // vte-0.15.0/src/ansi.rs:118 — `self` and a struct by `{:?}`.
        log::trace!("Scaling RGB by {} from {:?} to {:?}", rhs, this, result);

        let buf = String::from("52;c;");
        // vte-0.15.0/src/ansi.rs:1341 — `line!()`, a MACRO CALL as an argument,
        // and a borrow (`&buf`) that must not be taken.
        debug!("[unhandled osc_dispatch]: [{}] at line {}", &buf, line!());
    }
}

#[rustfmt::skip]
mod alacritty_and_wezterm {
    // alacritty_terminal-0.26.0/src/term/mod.rs:13
    use log::{debug, trace};

    pub fn forms() {
        let err = "regex too complex";

        // alacritty_terminal-0.26.0/src/term/search.rs:265
        debug!("    {err}");

        // (synthetic) `trace` rides the same import line.
        trace!("Term::resize dimensions unchanged");
    }
}

#[rustfmt::skip]
mod wezterm_forms {
    // wezterm-bidi-0.2.3/src/lib.rs:3
    use log::trace;

    pub fn forms() {
        let label = "resolve";

        // wezterm-bidi-0.2.3/src/lib.rs:450 — a format string that is ONLY
        // escapes and asterisks.
        trace!("\n**** resolve \n");

        // wezterm-bidi-0.2.3/src/lib.rs:501
        trace!("State: {}", label);

        let message = "unknown field";
        // wezterm-dynamic-0.2.1/src/error.rs:81
        log::warn!("{message}");
    }
}

// ===========================================================================
// The remaining verbatim `use log::…` lines, so invariant (1) is complete
// ===========================================================================

#[rustfmt::skip]
mod remaining_import_lines {
    // The `use log::…` spellings in the census that no module above reproduces
    // verbatim, each with the file:line it was copied from. With these, all
    // TWELVE distinct import lines in the twenty consumer trees are present —
    // that is invariant (1) in this file's module docs, and it is what
    // `#![deny(unused_imports)]` is asserting against: every imported name has
    // to be reached by a call below, or the file stops compiling.

    pub mod one {
        // gpu-allocator-0.28.0/src/vulkan/mod.rs:9
        use log::{debug, Level};
        pub fn forms() { let l = Level::Trace; debug!("dedicated block: {l}"); }
    }

    pub mod two {
        // gpu-allocator-0.28.0/src/d3d12/mod.rs:12
        use log::{debug, warn, Level};
        pub fn forms() { let l = Level::Info; debug!("{l}"); warn!("{l}"); }
    }

    pub mod three {
        // alacritty_terminal-0.26.0/src/tty/windows/conpty.rs:1
        use log::{info, warn};
        pub fn forms() { info!("spawned"); warn!("no pty"); }
    }

    pub mod four {
        // alacritty_terminal-0.26.0/src/event_loop.rs:14 — the same line is also
        // termwiz-0.23.3/src/escape/parser/mod.rs:7, reproduced in the termwiz
        // module above.
        use log::error;
        pub fn forms() { error!("io error"); }
    }

    pub mod five {
        // smithay-client-toolkit-0.19.2/src/output.rs:7 — the ONLY unqualified
        // import in smithay; its eighteen `target:` sites are all written
        // fully qualified, which is why this line has no call site up there.
        use log::warn;
        pub fn forms() {
            let event = "wl_output::Event::Done";
            // smithay-client-toolkit-0.19.2/src/output.rs:429
            warn!("Received {event:?} for dead wl_output");
        }
    }
}

// ===========================================================================
// TESTS — the runtime half
// ===========================================================================

/// EVERY consumer form above, executed.
///
/// Compiling them is most of the proof; running them is the rest, because a
/// `{}` expansion in tail or match-arm position could in principle typecheck
/// and still change what the surrounding function returns.
#[test]
fn every_consumer_form_compiles_and_runs() {
    assert_eq!(wgpu_hal_dxgi_exception::forms(), 0);
    assert_eq!(
        wgpu_hal_gles_egl::forms(),
        0,
        "the Trace gate must stay shut"
    );
    wgpu_hal_gles_mod::forms();
    assert_eq!(
        wgpu_hal_vulkan_instance::forms(),
        0x1,
        "only ERROR may be set; VERBOSE/INFO/WARNING are what max_level() gates"
    );
    wgpu_hal_dx12_command::forms();
    naga_forms::forms();
    wgpu_core_and_types::ray_tracing();
    assert_eq!(wgpu_core_and_types::from_env("7"), Some(7));
    assert_eq!(wgpu_core_and_types::from_env("nope"), None);
    wgpu_core_wrapper_sites::statement_forms(
        &wgpu_core_wrapper_sites::Desc {
            label: Some(String::from("aterm::vertices")),
            mapped_at_creation: true,
        },
        7,
    );
    wgpu_core_wrapper_sites::match_arm_tail(&Ok(()));
    wgpu_core_wrapper_sites::match_arm_tail(&Err(String::from("boom")));
    wgpu_hal_vulkan_swapchain::present(&wgpu_hal_vulkan_swapchain::Texture { index: 3 }, true);
    wgpu_hal_vulkan_swapchain::attribute_controls();
    gpu_allocator_allocator_mod::forms();
    gpu_allocator_free_list::forms();
    smithay_client_toolkit::forms();
    sctk_adwaita_buttons::forms();
    calloop_loop_logic::forms();
    tiny_skia_painter::forms();
    wayland_sys_client::forms();
    xkbcommon_dl::forms();
    gl_generator_registry_parse::forms();
    termwiz_parser::forms();
    vte_ansi::forms();
    alacritty_and_wezterm::forms();
    wezterm_forms::forms();
    remaining_import_lines::one::forms();
    remaining_import_lines::two::forms();
    remaining_import_lines::three::forms();
    remaining_import_lines::four::forms();
    remaining_import_lines::five::forms();
}

/// ARM THE TRIPWIRES. Without this test the two that follow are vacuous.
///
/// Proves, on a DIRECT call, that the counter moves and that the panic escapes
/// into `catch_unwind`. Only then does "the counter did not move" mean anything.
#[test]
fn tripwires_are_armed() {
    let before = CONTROL_EVALUATIONS.load(Ordering::SeqCst);
    let carried = control_tripwire(0xA7u8);
    assert_eq!(carried, 0xA7, "the tripwire must be an identity function");
    assert_eq!(
        CONTROL_EVALUATIONS.load(Ordering::SeqCst),
        before + 1,
        "the counter tripwire is DEAD: a direct call did not move it, so \
         `macros_never_evaluate_their_arguments` proves nothing"
    );

    // The panic tripwire. `catch_unwind` prints the panic message to stderr;
    // that noise is expected and is the proof the detonator works.
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = detonate();
    }));
    assert!(
        outcome.is_err(),
        "the panic tripwire is DEAD: `detonate()` returned instead of panicking"
    );
}

/// The macros evaluate NOTHING — the property that makes this shim identical to
/// a disabled upstream callsite rather than merely quiet.
#[test]
fn macros_never_evaluate_their_arguments() {
    let before = MACRO_EVALUATIONS.load(Ordering::SeqCst);

    // Every macro, in every argument position the census records.
    log::error!("{}", tripwire("error"));
    log::warn!(target: "sctk", "{}", detonate());
    log::info!("failed loading {} module", detonate());
    log::debug!("{:?} at line {}", tripwire(1u8), line!());
    log::trace!("{} {} {}", detonate(), tripwire(2u8), detonate());
    log::log!(tripwire(log::Level::Warn), "{}", detonate());
    log::log!(target: "sctk", log::Level::Error, "{}", detonate());

    // Inline captures, raw strings, doubled braces, trailing commas. The
    // detonator goes INSIDE the macro; an earlier draft of this file bound it
    // to a local first, which is a direct call, and the tripwire duly fired —
    // evidence, at the cost of one red test, that it is not decorative.
    log::debug!(r#"leak detected: {{ name: {} }}"#, detonate(),);

    // The same claim one macro layer out: an argument forwarded through
    // wgpu-core's `$($arg:tt)+` wrappers must not be evaluated either. The
    // wrapper is where a shim that matched a grammar rather than token trees
    // would first have had to look at what it was given.
    wgpu_core_wrapper_sites::never_evaluates();

    assert_eq!(
        MACRO_EVALUATIONS.load(Ordering::SeqCst),
        before,
        "a log macro evaluated its arguments; the shim is no longer identical \
         to a disabled upstream callsite"
    );
}

/// `log_enabled!` is a `bool` EXPRESSION and is always false.
///
/// Not vacuous: the `else` branch is what proves the expansion reaches the
/// condition at all rather than being optimized into the `if`'s shape.
#[test]
fn log_enabled_is_a_false_bool_expression() {
    let enabled: bool = log::log_enabled!(log::Level::Debug);
    assert!(
        !enabled,
        "log_enabled! must be false while max_level() is Off"
    );

    let taken = if log::log_enabled!(log::Level::Trace) {
        "then"
    } else {
        "else"
    };
    assert_eq!(taken, "else");

    // The other three upstream arms, none of which any consumer writes.
    let by_target: bool = log::log_enabled!(target: "sctk", log::Level::Error);
    assert!(!by_target);
}

/// THE SHARPEST PIN IN THE CRATE.
///
/// `max_level()` must read this crate's own static and answer `Off`. If it ever
/// delegated to `aterm_log::max_level()` — which `crates/aterm/src/main.rs` sets
/// to `Info` and `crates/aterm-gui/src/watchdog.rs` sets to `Trace` — the four
/// `wgpu-hal` comparisons reproduced here would flip, switching Vulkan
/// validation VERBOSE/INFO/WARNING severities on and adding a driver-side
/// callback per frame on the two cells this machine cannot run.
#[test]
fn max_level_is_off_from_this_crates_own_static() {
    assert_eq!(
        log::max_level(),
        log::LevelFilter::Off,
        "max_level() is not Off: either a setter was added to this crate, or it \
         was made to delegate to aterm_log — see THE SEAM in src/lib.rs"
    );

    // ARM THE COMPARISON. `>=` must be a live test, not something that is false
    // for every operand; the `Off` case is the one that must be true.
    let off_gate = log::max_level() >= log::LevelFilter::Off;
    assert!(off_gate, "`>=` on LevelFilter is not comparing anything");

    // The four real gates, in the order wgpu-hal writes them.
    let trace_gate = log::max_level() >= log::LevelFilter::Trace; // gles/egl.rs:424
    let debug_gate = log::max_level() >= log::LevelFilter::Debug; // vulkan/instance.rs:718
    let info_gate = log::max_level() >= log::LevelFilter::Info; //  vulkan/instance.rs:721
    let warn_gate = log::max_level() >= log::LevelFilter::Warn; //  vulkan/instance.rs:724
    assert!(
        !trace_gate,
        "egl.rs:424 would start enumerating EGL configs"
    );
    assert!(!debug_gate, "instance.rs:718 would add …::VERBOSE");
    assert!(!info_gate, "instance.rs:721 would add …::INFO");
    assert!(!warn_gate, "instance.rs:724 would add …::WARNING");
}

/// THE TRANSCRIBED TABLE ORACLE.
///
/// Every value here was read out of `log-0.4.32`'s own source at the cited line
/// and written down; this test asserts the shim reproduces it. It is
/// transcription, not co-execution — see this file's module docs for why a live
/// differential is structurally impossible for a `[patch.crates-io]` target.
#[test]
fn upstream_table_oracle() {
    use log::{Level, LevelFilter};

    // log-0.4.32/src/lib.rs:472-499 and :665-680 — the discriminants, which are
    // deliberately aligned so `*self as usize` comparisons work ACROSS the two
    // types. Renumbering either enum silently inverts every `max_level()` gate.
    assert_eq!(Level::Error as usize, 1);
    assert_eq!(Level::Warn as usize, 2);
    assert_eq!(Level::Info as usize, 3);
    assert_eq!(Level::Debug as usize, 4);
    assert_eq!(Level::Trace as usize, 5);
    assert_eq!(LevelFilter::Off as usize, 0);
    assert_eq!(LevelFilter::Error as usize, 1);
    assert_eq!(LevelFilter::Warn as usize, 2);
    assert_eq!(LevelFilter::Info as usize, 3);
    assert_eq!(LevelFilter::Debug as usize, 4);
    assert_eq!(LevelFilter::Trace as usize, 5);

    // log-0.4.32/src/lib.rs:460
    //   static LOG_LEVEL_NAMES: [&str; 6] =
    //       ["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"];
    // Upstream's `as_str` is `LOG_LEVEL_NAMES[*self as usize]`; the shim writes
    // a total match. Asserting index-by-index is what keeps the two spellings
    // of one table from drifting.
    let names = ["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"];
    for filter in LevelFilter::iter() {
        assert_eq!(filter.as_str(), names[filter as usize]);
    }
    for level in Level::iter() {
        assert_eq!(level.as_str(), names[level as usize]);
        assert_eq!(level.to_level_filter().as_str(), level.as_str());
    }

    // log-0.4.32/src/lib.rs:528-530 and :678-681 — `fmt.pad(self.as_str())`,
    // NOT `write_str`. This is divergence reason 2 from the shim's module docs:
    // `aterm_log::Level`'s Display ignores width, so it would answer "WARN".
    assert_eq!(format!("{}", Level::Warn), "WARN");
    assert_eq!(format!("{:>7}", Level::Warn), "   WARN");
    assert_eq!(format!("{:<7}|", Level::Warn), "WARN   |");
    assert_eq!(format!("{:.2}", Level::Trace), "TR");
    assert_eq!(format!("{:>7}", LevelFilter::Off), "    OFF");

    // log-0.4.32/src/lib.rs:515-526 and :665-676 — `FromStr`, case-insensitive,
    // NO TRIMMING, and the deliberate asymmetry: `Level` rejects "off" (its scan
    // starts at index 1), `LevelFilter` accepts it. This is divergence reason 3:
    // `aterm_log::LevelFilter::parse` trims, so it answers `Some(Warn)` for the
    // padded input below where upstream and this crate answer `Err`.
    assert_eq!("warn".parse::<Level>(), Ok(Level::Warn));
    assert_eq!("WARN".parse::<Level>(), Ok(Level::Warn));
    assert_eq!("WaRn".parse::<Level>(), Ok(Level::Warn));
    assert!("off".parse::<Level>().is_err());
    assert!("warn ".parse::<Level>().is_err(), "upstream does not trim");
    assert!("asdf".parse::<Level>().is_err());
    assert_eq!("off".parse::<LevelFilter>(), Ok(LevelFilter::Off));
    assert_eq!("OFF".parse::<LevelFilter>(), Ok(LevelFilter::Off));
    assert_eq!("trace".parse::<LevelFilter>(), Ok(LevelFilter::Trace));
    assert!(
        " off".parse::<LevelFilter>().is_err(),
        "upstream does not trim"
    );
    assert!("asdf".parse::<LevelFilter>().is_err());

    // log-0.4.32/src/lib.rs:464-465 — the `Display` text of ParseLevelError.
    let err = "asdf".parse::<Level>().unwrap_err();
    assert_eq!(
        err.to_string(),
        "attempted to convert a string that doesn't match an existing log level"
    );

    // log-0.4.32/src/lib.rs:544 (`Level::max`), :670 region (`LevelFilter::max`)
    assert_eq!(Level::max(), Level::Trace);
    assert_eq!(LevelFilter::max(), LevelFilter::Trace);

    // log-0.4.32/src/lib.rs:552-557, :700 region — the two conversions.
    assert_eq!(Level::Info.to_level_filter(), LevelFilter::Info);
    assert_eq!(LevelFilter::Info.to_level(), Some(Level::Info));
    assert_eq!(LevelFilter::Off.to_level(), None);

    // log-0.4.32/src/lib.rs:576-579 and :700 region — iteration order is most
    // severe first, and the counts differ by one because only LevelFilter has
    // `Off`.
    assert_eq!(Level::iter().count(), 5);
    assert_eq!(Level::iter().next(), Some(Level::Error));
    assert_eq!(Level::iter().last(), Some(Level::Trace));
    assert_eq!(LevelFilter::iter().count(), 6);
    assert_eq!(LevelFilter::iter().next(), Some(LevelFilter::Off));

    // log-0.4.32/src/lib.rs:594-604 and :615-625 — saturating severity walks.
    assert_eq!(Level::Info.increment_severity(), Level::Debug);
    assert_eq!(Level::Trace.increment_severity(), Level::Trace);
    assert_eq!(Level::Info.decrement_severity(), Level::Warn);
    assert_eq!(Level::Error.decrement_severity(), Level::Error);
    assert_eq!(LevelFilter::Off.increment_severity(), LevelFilter::Error);
    assert_eq!(LevelFilter::Error.decrement_severity(), LevelFilter::Off);
    assert_eq!(LevelFilter::Off.decrement_severity(), LevelFilter::Off);
}

/// The four cross-type comparison impls (log-0.4.32/src/lib.rs:501-513,
/// :651-663). Unused in today's graph — every comparison found is same-type —
/// and present because a `Level` being directly comparable to a `LevelFilter` is
/// the idiom upstream's type documentation advertises, so a wgpu or naga bump
/// could introduce one on a cell this machine cannot compile.
#[test]
fn level_and_level_filter_compare_across_types() {
    use log::{Level, LevelFilter};

    assert!(Level::Error == LevelFilter::Error);
    assert!(LevelFilter::Error == Level::Error);
    assert!(Level::Error != LevelFilter::Off);
    assert!(Level::Error > LevelFilter::Off);
    assert!(LevelFilter::Off < Level::Error);
    assert!(Level::Trace > LevelFilter::Debug);
    assert!(LevelFilter::Trace >= Level::Trace);

    // The shape a future consumer would most plausibly write, and the reason
    // these impls are here: it does not compile without them.
    for level in Level::iter() {
        let would_emit = level <= log::max_level();
        assert!(!would_emit, "{level} would emit while max_level() is Off");
    }
}

/// THE ONE DOCUMENTED DELTA FROM UPSTREAM, pinned rather than left to be
/// rediscovered.
///
/// Upstream's `__log!` is `let lvl = $lvl; if lvl <= … { … }`
/// (log-0.4.32/src/macros.rs:119-146), so a disabled callsite DOES evaluate the
/// level expression — only the format arguments are skipped. This crate discards
/// the whole token tree, so it does not. That is unobservable at all thirteen
/// `log!` / `log_enabled!` sites in today's graph, because every one passes a
/// plain `Copy` local, closure parameter or literal with no side effect
/// (enumerated in the shim's module docs). It would become observable for a
/// future `log!(compute_level(), …)`.
///
/// The five level macros have NO such delta: upstream passes them a constant
/// `Level::Error`, so there was never anything to evaluate.
#[test]
fn log_and_log_enabled_do_not_read_their_level_argument() {
    let before = MACRO_EVALUATIONS.load(Ordering::SeqCst);

    log::log!(tripwire(log::Level::Error), "message");
    let _enabled: bool = log::log_enabled!(tripwire(log::Level::Debug));

    assert_eq!(
        MACRO_EVALUATIONS.load(Ordering::SeqCst),
        before,
        "the level argument WAS evaluated — that is upstream's behaviour, not \
         this crate's; if it is now intentional, update the module docs, which \
         currently claim the opposite"
    );
}

/// The surface NO CONSUMER TOUCHES, and the surface deliberately ABSENT.
///
/// The first half: items this crate exports that nothing in the graph names.
/// They are here because the shim's module docs justify them, and until this
/// test existed they had no coverage at all.
///
/// The second half cannot be a test, because the whole point is that the items
/// do not exist. It is written down instead: `Log`, `Record`, `RecordBuilder`,
/// `Metadata`, `MetadataBuilder`, `set_logger`, `set_boxed_logger`,
/// `set_max_level`, `logger()`, `SetLoggerError`, `STATIC_MAX_LEVEL`, `kv` and
/// `__private_api` are ABSENT ON PURPOSE. A no-op `set_logger` would let a
/// future consumer install a logger, compile, and receive nothing forever;
/// omitted, the same code fails to build with an error naming the missing item.
/// If you add one of them, delete this paragraph — do not quietly widen it.
#[test]
fn exported_but_unused_surface_still_works() {
    use log::{Level, LevelFilter, ParseLevelError};

    // `ParseLevelError`'s derives (log-0.4.32/src/lib.rs:1565-1566).
    let a: ParseLevelError = "asdf".parse::<Level>().unwrap_err();
    let b: ParseLevelError = "qwer".parse::<LevelFilter>().unwrap_err();
    assert_eq!(a, b);
    assert!(format!("{a:?}").contains("ParseLevelError"));

    // `Level`'s derived `Ord`/`Hash`, which nothing in the graph uses but which
    // upstream's derive list includes.
    let mut levels: Vec<Level> = Level::iter().collect();
    levels.sort_unstable();
    assert_eq!(levels.first(), Some(&Level::Error));
    let mut set = std::collections::HashSet::new();
    assert!(set.insert(Level::Warn));
    assert!(!set.insert(Level::Warn));
}
