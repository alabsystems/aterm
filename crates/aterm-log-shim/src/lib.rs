// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `log` — aterm's first-party replacement for the upstream `log` facade.
//! Every macro in it expands to nothing and [`max_level`] is `Off`.
//!
//! This crate is published into the build under the package name `log` (the
//! directory is `crates/aterm-log-shim`; see the manifest for why the two names
//! differ, and why this one needed a suffix) so that `[patch.crates-io]` can
//! redirect the **twenty** third-party consumers in aterm's graph at code we
//! own. It replaces `log 0.4.32` — 6,286 lines of Rust in the package, 5,698 of
//! them under `src/` — with a six-element lattice and seven macros that do
//! nothing at all.
//!
//! # The consumers, all twenty, per cell
//!
//! The brief this crate was built from named seven. Re-derived here — and the
//! re-derivation is the point, because eight of them are absent from the mac
//! cell entirely and a ninth, `wgpu-hal`, is compiled here with the three
//! backends that use `log` hardest cfg'd out:
//!
//! ```text
//! cargo tree -e all --workspace --target <cell> --depth 2 \
//!   -i 'https://github.com/rust-lang/crates.io-index#log@0.4.32'
//! ```
//!
//! THE SOURCE-QUALIFIED SPEC IS REQUIRED, and the bare `-i log` that reads more
//! naturally is a TRAP. Once this crate exists, the workspace holds two packages
//! named `log` — this path member and the registry copy — so `-i log` is
//! ambiguous; cargo resolves it to the member, which nothing depends on, and
//! prints NOTHING AT ALL rather than an error. A reviewer who runs the short
//! form sees an empty consumer list and concludes the package already left the
//! graph. It has not: until the `[patch.crates-io]` row lands, every consumer
//! below still resolves to the registry copy, and the command above shows them.
//!
//! * **all four cells** (mac-arm, windows-msvc, linux-gnu, wasm32) — `naga`,
//!   `wgpu`, `wgpu-core`, `wgpu-hal`, `wgpu-types`, `rustybuzz`,
//!   `rustls-platform-verifier`, and through `[dev-dependencies]` of
//!   `aterm-bench` / `aterm-render`: `alacritty_terminal`, `termwiz`, `vte`,
//!   `wezterm-bidi`, `wezterm-dynamic`
//! * **windows + linux** — `gpu-allocator`
//! * **linux only** — `calloop`, `sctk-adwaita`, `smithay-client-toolkit`,
//!   `tiny-skia`, `wayland-sys`, `xkbcommon-dl`
//! * **windows build-dependency** — `gl_generator` (under `glutin_wgl_sys`),
//!   the graph's only `#[macro_use] extern crate log;`
//!
//! `-e all`, not `-e normal`: the five dev-dependency consumers contribute 146
//! macro sites and the only match-arm and tail-expression forms in the census,
//! and a `-e normal` census does not see one of them. `--depth 2`, not
//! `--depth 1`: `-e all` prints a feature tree, so the package rows are one
//! level further down than the `-e normal` form.
//!
//! # Why a no-op is *correct*, and why that is proved rather than argued
//!
//! The claim is not "nobody reads these logs". It is that a no-op is
//! **behaviourally identical** to what ships today, and the identity rests on
//! four facts, each of which can be re-checked:
//!
//! 1. **No logger crate is in the graph, on any cell.** Re-check it:
//!
//!    ```text
//!    cargo tree -e all --workspace --target <cell> \
//!      | grep -cE '\b(env_logger|android_logger|fern|simplelog|console_log|tracing-log|log4rs|femme|flexi_logger|stderrlog|pretty_env_logger|systemd-journal-logger|log-panics|kv-log-macro|structured-logger) v'
//!    ```
//!
//!    It returns 0 on all four cells. Use `-e all`, not `-e normal`: a logger
//!    added to any workspace crate's `[dev-dependencies]` would keep the narrow
//!    count at 0 while this shim silently blanked that test's output.
//! 2. **Nothing calls `log::set_logger` or `log::set_max_level`.** Grepped
//!    across all twenty consumer source trees: zero hits for `set_logger`,
//!    `set_boxed_logger` and `impl Log for`. Every `set_max_level` in the
//!    workspace is `aterm_log::set_max_level` — a *different crate*, which is
//!    the whole subject of the "why not delegate" section below.
//! 3. **Therefore `log::max_level()` is `Off` for the entire process
//!    lifetime.** Upstream's backing global is
//!    `static MAX_LOG_LEVEL_FILTER: AtomicUsize = AtomicUsize::new(0)`
//!    (log-0.4.32/src/lib.rs:458), `0` is `LevelFilter::Off`, and (2) says
//!    nothing ever stores anything else.
//! 4. **A disabled callsite formats nothing.** Upstream's `__log!` is
//!    `let lvl = $lvl; if lvl <= STATIC_MAX_LEVEL && lvl <= max_level() { … }`
//!    (log-0.4.32/src/macros.rs:119-146) — the `format_args!` and every
//!    argument it names live *inside* the `if`. With `max_level()` at `Off`,
//!    `lvl <= Off` is false for all five levels, so the body never runs.
//!
//! So the shipped behaviour of every `log` invocation in aterm's graph is:
//! evaluate the level expression, then nothing. Allocate nothing, format
//! nothing, emit nothing. Macros that expand to `{}` reproduce that, with one
//! documented delta, below.
//!
//! ## The one delta: `log!` and `log_enabled!` stop reading their level
//!
//! Upstream evaluates `$lvl` even on a disabled callsite — it is bound before
//! the `if`. This crate discards the token tree, so it does not. That is
//! observationally identical **at every site in today's graph**, and that is a
//! measurement rather than a guarantee: all thirteen sites pass a plain `Copy`
//! local, closure parameter, or literal with no side effect —
//!
//! * `level` — naga `front/spv/mod.rs:853`, `valid/analyzer.rs:912`,
//!   `front/wgsl/parse/directive.rs:47`; wgpu-hal `auxil/dxgi/exception.rs:85`,
//!   `vulkan/instance.rs:98,117,130,150`
//! * `log_severity` — wgpu-hal `gles/egl.rs:78`, `gles/mod.rs:1106`
//! * `log_level` — gpu-allocator `dedicated_block_allocator/mod.rs:120`,
//!   `free_list_allocator/mod.rs:395`
//! * `log::Level::Debug` — naga `proc/overloads/list.rs:98`, the sole
//!   `log_enabled!`
//!
//! A future consumer writing `log!(compute_level(), …)` would lose that call.
//! `tests/consumer_forms.rs` pins the delta explicitly rather than leaving it to
//! be discovered, and the five level macros have **no** delta at all: upstream
//! passes them a constant `Level::Error`, so there was never anything to read.
//!
//! # The corollary: installing a logger means retiring this shim
//!
//! Facts (1) and (2) are premises, not laws. The moment anyone installs a
//! logger into crates.io `log` — an `env_logger` dependency, a `set_logger`
//! call, a `tracing-log` bridge — the argument above collapses, and this shim
//! will produce **no output whatsoever**. It will not fail to compile and it
//! will not warn; wgpu's and naga's diagnostics simply will not be there.
//!
//! If you want them, there are two moves and they are not the same one:
//!
//! * *Retire this crate.* Drop the `log` row from the root
//!   `[patch.crates-io]` table so the real facade resolves again. This is the
//!   right move if you want an ordinary logging setup.
//! * *Flip this crate to `forward` mode.* See "The seam", at the bottom of this
//!   file. This is an OWNER DECISION with real consequences, not a cleanup.
//!
//! # Why this does NOT delegate to `crates/aterm-log`
//!
//! aterm already ships a first-party logging facade with `Level`, `LevelFilter`,
//! `Metadata`, `Record`, `Log`, `set_logger`, `set_max_level` and `max_level` —
//! `crates/aterm-log`, which fifteen workspace crates depend on. The extraction
//! wave's default is to delegate to code like that rather than rewrite it. This
//! target is the exception, and the exception is wide enough to swallow the
//! rule. Four measured reasons, worst first:
//!
//! 1. **`max_level()` would change what the Vulkan driver is asked to do.**
//!    This is not a formatting concern. `wgpu-hal` reads `log::max_level()` four
//!    times to gate *real work*:
//!
//!    ```text
//!    wgpu-hal-29.0.3/src/vulkan/instance.rs:718  >= Debug  => severity |= …::VERBOSE
//!    wgpu-hal-29.0.3/src/vulkan/instance.rs:721  >= Info   => severity |= …::INFO
//!    wgpu-hal-29.0.3/src/vulkan/instance.rs:724  >= Warn   => severity |= …::WARNING
//!    wgpu-hal-29.0.3/src/gles/egl.rs:424         >= Trace  => get_config_count/get_configs loop
//!    ```
//!
//!    The first three build the `vk::DebugUtilsMessageSeverityFlagsEXT` bitmask
//!    handed to the Vulkan driver. All four are `false` today because
//!    upstream's global is `Off`. `aterm_log`'s global is **not** `Off`:
//!    `crates/aterm/src/main.rs` sets `Info` and
//!    `crates/aterm-gui/src/watchdog.rs` sets `Trace`. A `max_level()` that
//!    forwarded there would switch VERBOSE / INFO / WARNING severities on in
//!    the validation callback — extra driver callbacks, extra allocations, per
//!    frame — on the two cells that cannot be compiled or run on the machine
//!    that develops this. So [`max_level`] reads **this crate's own static**,
//!    which nothing in this crate writes, and `tests/consumer_forms.rs` pins it.
//! 2. **`Display for Level` would silently change formatting.** Upstream is
//!    `fmt.pad(self.as_str())` (log-0.4.32/src/lib.rs:528-530), which honours
//!    width, alignment and precision. `aterm_log`'s is `f.write_str("WARN")`,
//!    which ignores them: `format!("{:>7}", Level::Warn)` is `"   WARN"`
//!    upstream and `"WARN"` there, with no error anywhere. This crate
//!    reproduces `fmt.pad`.
//! 3. **`LevelFilter::parse` would leak into the `log::` namespace.**
//!    `aterm_log::LevelFilter` has an inherent `parse(&str) -> Option<Self>`
//!    that `.trim()`s its input; upstream has `FromStr`, which does not, and
//!    which rejects `"off"` for `Level` while accepting it for `LevelFilter`.
//!    Re-exporting a type re-exports its inherent methods, so delegation would
//!    publish `log::LevelFilter::parse` with semantics upstream does not have —
//!    `"warn ".parse()` answering `Err` in one and `Some(Warn)` in the other,
//!    with both APIs in scope and no compile error to separate them. This crate
//!    implements upstream's `FromStr` exactly and has no `parse`.
//! 4. **`no_std`.** `aterm_log` is std-only (`std::sync::OnceLock`,
//!    `std::borrow::Cow`, `std::error::Error`, a whole `pub mod env` over
//!    `std::env`). Seven consumers are `#![no_std]` — naga, wgpu, wgpu-core,
//!    wgpu-hal, wgpu-types, rustybuzz, tiny-skia — and none enables `log/std`.
//!
//! Reasons 2 and 3 are the decisive ones, because they are cases where
//! delegating would ship an API that is *wrong*, not merely inelegant. Reason 1
//! is the loudest. Reason 4 is real but would not have broken today's build on
//! its own: all four cells have `std`.
//!
//! # Surface, and the rule that decides it
//!
//! One rule, checkable in a single pass:
//!
//! > **Every pure function of the level lattice is present and exact.
//! > Everything whose answer depends on a logger being installed is absent —
//! > except [`max_level`], which cannot be absent because four call sites name
//! > it, and is therefore pinned to the value it has today.**
//!
//! Present: [`Level`], [`LevelFilter`], [`ParseLevelError`], [`max_level`], and
//! the seven macros [`error!`], [`warn!`], [`info!`], [`debug!`], [`trace!`],
//! [`log!`], [`log_enabled!`].
//!
//! Absent, and the absence is the safety feature: `Log`, `Record`,
//! `RecordBuilder`, `Metadata`, `MetadataBuilder`, `set_logger`,
//! `set_boxed_logger`, `set_max_level`, `logger()`, `SetLoggerError`,
//! `STATIC_MAX_LEVEL`, `kv`, `__private_api`. None is named anywhere in the
//! twenty consumer trees (grepped: zero hits for `__private_api`, `log::kv`,
//! `kv::Source`, `STATIC_MAX_LEVEL`, `set_logger`, `set_boxed_logger`,
//! `impl Log for`, `log::Record`, `log::Metadata`). Exporting no-op versions
//! would be worse than omitting them: a consumer that called `set_logger` would
//! then compile and receive *nothing*, forever, silently. Omitted, the same code
//! fails to build with an error naming the exact missing item — which is the
//! loud failure this crate wants. `STATIC_MAX_LEVEL` is omitted for the same
//! reason even though its value is pure: a reader who found it would conclude
//! that events at or below it are emitted.
//!
//! ## The exported NAME SET is a strict subset of upstream's, on purpose
//!
//! `gpu-allocator-0.28.0/src/allocator/mod.rs` has two glob imports three lines
//! apart — `use log::*;` (line 8) and `use crate::result::*;` (line 10) — and
//! `crate::result` exports `AllocationError` and `Result`. Two globs supplying
//! one name is an error at the *use site*, in a file that only builds on
//! Windows and Linux. So this crate exports no `Result`, no `Error`, no
//! `AllocationError`, and none of `aterm_log`'s extras (`env`,
//! `sanitize_record`, `should_truncate`, `MAX_LOG_BYTES`, `MAX_RECORD_BYTES`,
//! `__log`). Every name it does export is a name upstream `log` exports.
//!
//! # `unused_variables` in the consumers, and why it is safe *here*
//!
//! Because the macros discard their token trees, a local that a consumer only
//! ever mentions inside a macro argument becomes genuinely unused, and rustc
//! says so. Every affected file is a registry crate (compiled with
//! `--cap-lints allow`) or the Windows build-dep `gl_generator` (also registry).
//! No first-party aterm crate depends on crates.io `log` — grepped across every
//! workspace `Cargo.toml`, the only hits are `crates/aterm-tracing`'s inert
//! `log = []` / `log-always = ["log"]` *feature names* — and `vendor/winit`, the
//! one path dependency that does not get `--cap-lints`, contains zero `log::`
//! references. So the workspace's `-D warnings` gate is untouched. Re-check that
//! claim if any aterm crate ever takes a `log` dependency.
//!
//! The alternative — a `let _ = &$arg;` per argument to keep names "used" — was
//! rejected for the same reason `crates/aterm-tracing` rejected it: taking a
//! reference *is* evaluation, and several of these arguments are
//! `unsafe { CStr::from_ptr(…) }.to_string_lossy()` inside a driver callback.
//!
//! # Proof of surface
//!
//! macOS resolves `wgpu-hal` to `default, metal, portable-atomic` — no dx12, no
//! gles, no vulkan. So a local build reaches the five LEVEL macros in bulk
//! (naga, rustybuzz, and, under `cargo test`, the five dev-dependency
//! consumers) and, of the hard forms, only naga's three `log!` sites and its one
//! `log_enabled!`. Invisible here: all seven wgpu-hal `log!` sites, all four
//! `max_level()` sites, the `const &[(&str, log::Level)]` array, the five
//! `level == log::Level::…` comparisons, gpu-allocator's glob and its two
//! raw-string `log!`s, smithay's eighteen `target:` forms, and gl_generator's
//! `#[macro_use] extern crate log;`.
//!
//! `tests/consumer_forms.rs` is therefore the deliverable, not an extra: every
//! invocation form in the twenty consumer trees, copied verbatim with a comment
//! naming its file and line. Compiling it *is* the proof that this surface
//! accepts what the real consumers write. It also carries the transcribed table
//! oracle (see the manifest's `[dev-dependencies]` comment for why a live
//! differential is structurally impossible for a `[patch.crates-io]` target) and
//! the armed tripwires for the zero-evaluation claim.
//!
//! ## The one piece of evidence that is not transcription: a real patch, run
//!
//! `tests/consumer_forms.rs` proves this surface accepts *copies* of what the
//! consumers write. To prove it accepts what they ACTUALLY write, the shim was
//! put behind a live `[patch.crates-io]` in a throwaway workspace — outside this
//! repo, so nothing here was touched — and the real registry sources were
//! compiled against it. Everything reachable on this cell went green:
//!
//! ```text
//! mkdir -p /tmp/patchproof/src && cd /tmp/patchproof
//! cat > Cargo.toml <<'EOF'
//! [package]
//! name = "patchproof"
//! version = "0.1.0"
//! edition = "2021"
//! [dependencies]
//! log = "0.4.29"
//! naga = { version = "=29.0.3", features = ["wgsl-in", "spv-in"] }
//! rustybuzz = "0.20.1"
//! vte = "0.15.0"
//! termwiz = "0.23.3"
//! wezterm-bidi = "0.2.3"
//! wezterm-dynamic = "0.2.1"
//! alacritty_terminal = "0.26.0"
//! [workspace]
//! [patch.crates-io]
//! log = { path = "<this repo>/crates/aterm-log-shim" }
//! EOF
//! # plus a .cargo/config.toml carrying this repo's `-Ztrust-verify=off` table,
//! # because a workspace outside the repo verifies strictly and drowns in
//! # soundness-gate banners.
//! cargo check      # => 0 errors
//! ```
//!
//! `cargo tree -i log` confirms the patch took there (in THAT workspace the
//! name is unambiguous, because the patch leaves exactly one `log`; here it is
//! not — see the trap noted above). The single `log v0.4.32` node is
//! the path to this directory, and NAGA 29.0.3 — the crate carrying three of
//! the twelve `log!` sites, the graph's only `log_enabled!`, and `log::Level` in
//! a closure parameter type — compiles from scratch against it. That is eight of
//! the twenty consumers proved by execution rather than by copy. The other
//! twelve are Windows-only, Linux-only, or inside `wgpu-hal` backends this cell
//! does not build; for those, the oracle file is all the evidence there is.
//!
//! The run also DEMONSTRATED the documented `unused_variables` divergence: the
//! throwaway crate's own `pub fn shout(level: log::Level)`, whose `level` is
//! mentioned only inside `log!`, drew exactly one warning. Registry crates get
//! `--cap-lints allow` and never see it; a first-party crate would.

#![no_std]
// `no_std` unconditionally, and not behind the `std` feature — the same choice
// `crates/aterm-tracing` made and for a sharper reason. Seven of the twenty
// consumers are `#![no_std]` (naga, wgpu, wgpu-core, wgpu-hal, wgpu-types,
// rustybuzz, tiny-skia) and `gpu-allocator` is
// `#![cfg_attr(not(feature = "std"), no_std)]`; none of them enables `log/std`,
// measured on all four cells. Nothing in this crate needs an allocator or an
// OS, so the simplest way to guarantee it keeps working for them is to never
// link `std` at all and let the `std` feature be an accepted no-op — which is
// also what upstream's own `std` feature is for this surface, since the only
// thing it gates there is `impl error::Error for ParseLevelError`. Integration
// tests and doctests are separate crates and get `std` as usual.

use core::cmp;
use core::fmt;
use core::str::FromStr;
use core::sync::atomic::{AtomicUsize, Ordering};

/// The level names, in discriminant order, transcribed from
/// `log-0.4.32/src/lib.rs:460`:
///
/// ```text
/// static LOG_LEVEL_NAMES: [&str; 6] = ["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"];
/// ```
///
/// Index *is* the discriminant for both enums, which is what makes
/// [`FromStr`] a linear scan in both directions. `tests/consumer_forms.rs`
/// asserts every entry against [`Level::as_str`] / [`LevelFilter::as_str`], so
/// the two spellings of the same table cannot drift apart.
static LOG_LEVEL_NAMES: [&str; 6] = ["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"];

/// Upstream's `LEVEL_PARSE_ERROR`, transcribed from
/// `log-0.4.32/src/lib.rs:464-465`. It is the `Display` text of
/// [`ParseLevelError`], and a caller could plausibly match on it.
static LEVEL_PARSE_ERROR: &str =
    "attempted to convert a string that doesn't match an existing log level";

// ---------------------------------------------------------------------------
// The level lattice
// ---------------------------------------------------------------------------

/// An enum representing the available verbosity levels of the logger.
///
/// THE DISCRIMINANTS ARE LOAD-BEARING AND ARE NOT TO BE "CLEANED UP".
/// `Level` starts at 1 and [`LevelFilter`] starts at 0 precisely so that the
/// two line up and `*self as usize` comparisons work across the pair — that is
/// upstream's own comment at `log-0.4.32/src/lib.rs:481-484`, and it is what
/// makes `log::max_level() >= log::LevelFilter::Debug` mean what wgpu-hal
/// thinks it means. Renumbering either enum inverts that test silently.
///
/// The derive list is upstream's, verbatim (`log-0.4.32/src/lib.rs:473`). Every
/// one of them is exercised by a real consumer: `Copy` + `Clone` because
/// wgpu-hal reads a `level` twice and passes it by value into `log!`;
/// `PartialEq` because `level == log::Level::…` appears five times in wgpu-hal
/// (`auxil/dxgi/exception.rs:73,79,89`, `gles/mod.rs:1113`,
/// `vulkan/instance.rs:155`); and const-constructibility because
/// the same file writes `const MESSAGE_PREFIXES: &[(&str, log::Level)]`.
#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Level {
    /// The "error" level.
    ///
    /// Designates very serious errors.
    Error = 1,
    /// The "warn" level.
    ///
    /// Designates hazardous situations.
    Warn,
    /// The "info" level.
    ///
    /// Designates useful information.
    Info,
    /// The "debug" level.
    ///
    /// Designates lower priority information.
    Debug,
    /// The "trace" level.
    ///
    /// Designates very low priority, often extremely verbose, information.
    Trace,
}

impl Level {
    /// The most verbose logging level.
    #[must_use]
    #[inline]
    pub const fn max() -> Self {
        Self::Trace
    }

    /// Convert the `Level` to the equivalent [`LevelFilter`].
    ///
    /// Written as a total match rather than upstream's
    /// `LevelFilter::from_usize(*self as usize).unwrap()`: same answer on every
    /// input, no panic branch for the Trust verifier to carry. Reachable in the
    /// graph only behind two `cfg`s — `rustls-platform-verifier-0.6.2`'s
    /// `src/tests/ffi.rs:46` calls it, under
    /// `#[cfg(any(test, feature = "ffi-testing"))] mod tests;` and then
    /// `#[cfg(target_os = "android")]`, and `ffi-testing` is enabled on no cell.
    #[must_use]
    #[inline]
    pub const fn to_level_filter(&self) -> LevelFilter {
        match self {
            Self::Error => LevelFilter::Error,
            Self::Warn => LevelFilter::Warn,
            Self::Info => LevelFilter::Info,
            Self::Debug => LevelFilter::Debug,
            Self::Trace => LevelFilter::Trace,
        }
    }

    /// The string representation of the `Level`.
    ///
    /// The same string the [`fmt::Display`] impl produces. Upstream indexes
    /// `LOG_LEVEL_NAMES[*self as usize]`; a total match is the same function
    /// without the bounds check, and the test file asserts the two agree
    /// entry-for-entry.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// Iterate through all supported logging levels, most severe first.
    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::Error,
            Self::Warn,
            Self::Info,
            Self::Debug,
            Self::Trace,
        ]
        .into_iter()
    }

    /// The next-higher-severity `Level`, saturating at [`Level::Trace`].
    #[must_use]
    #[inline]
    pub const fn increment_severity(&self) -> Self {
        match self {
            Self::Error => Self::Warn,
            Self::Warn => Self::Info,
            Self::Info => Self::Debug,
            Self::Debug | Self::Trace => Self::Trace,
        }
    }

    /// The next-lower-severity `Level`, saturating at [`Level::Error`].
    #[must_use]
    #[inline]
    pub const fn decrement_severity(&self) -> Self {
        match self {
            Self::Error | Self::Warn => Self::Error,
            Self::Info => Self::Warn,
            Self::Debug => Self::Info,
            Self::Trace => Self::Debug,
        }
    }
}

impl fmt::Display for Level {
    /// `fmt.pad`, NOT `write_str`, and the difference is the whole reason this
    /// type is not a re-export of `aterm_log::Level`.
    ///
    /// `pad` honours width, alignment and precision; `write_str` ignores them.
    /// `format!("{:>7}", Level::Warn)` is `"   WARN"` here and upstream, and
    /// would be `"WARN"` through `aterm_log`. Transcribed from
    /// `log-0.4.32/src/lib.rs:528-530`; pinned in `tests/consumer_forms.rs`.
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.pad(self.as_str())
    }
}

impl FromStr for Level {
    type Err = ParseLevelError;

    /// Upstream's semantics exactly (`log-0.4.32/src/lib.rs:515-526`): ASCII
    /// case-insensitive, **no trimming**, and `"OFF"` is REJECTED — the scan
    /// starts at index 1, skipping `LOG_LEVEL_NAMES[0]`.
    ///
    /// The no-trimming half matters: `aterm_log::LevelFilter::parse` trims, so
    /// `"warn "` answers `Some(Warn)` there and `Err` here. Two functions, two
    /// answers, no compile error to tell them apart — which is why this crate
    /// implements `FromStr` itself rather than re-exporting the other one.
    fn from_str(level: &str) -> Result<Self, Self::Err> {
        for (idx, name) in LOG_LEVEL_NAMES.iter().enumerate().skip(1) {
            if name.eq_ignore_ascii_case(level) {
                return level_from_usize(idx).ok_or(ParseLevelError(()));
            }
        }
        Err(ParseLevelError(()))
    }
}

/// An enum representing the available verbosity level filters of the logger.
///
/// A `LevelFilter` may be compared directly to a [`Level`]; see the four
/// cross-type impls below. Its discriminants start at 0 so `Off` sorts below
/// every `Level` — see the note on [`Level`].
#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum LevelFilter {
    /// A level lower than all log levels.
    Off,
    /// Corresponds to the `Error` log level.
    Error,
    /// Corresponds to the `Warn` log level.
    Warn,
    /// Corresponds to the `Info` log level.
    Info,
    /// Corresponds to the `Debug` log level.
    Debug,
    /// Corresponds to the `Trace` log level.
    Trace,
}

impl LevelFilter {
    /// The most verbose logging level filter.
    #[must_use]
    #[inline]
    pub const fn max() -> Self {
        Self::Trace
    }

    /// The equivalent [`Level`], or `None` for [`LevelFilter::Off`].
    #[must_use]
    #[inline]
    pub const fn to_level(&self) -> Option<Level> {
        match self {
            Self::Off => None,
            Self::Error => Some(Level::Error),
            Self::Warn => Some(Level::Warn),
            Self::Info => Some(Level::Info),
            Self::Debug => Some(Level::Debug),
            Self::Trace => Some(Level::Trace),
        }
    }

    /// The string representation of the `LevelFilter`.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// Iterate through all supported filtering levels, most restrictive first.
    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::Off,
            Self::Error,
            Self::Warn,
            Self::Info,
            Self::Debug,
            Self::Trace,
        ]
        .into_iter()
    }

    /// The next-higher-verbosity filter, saturating at [`LevelFilter::Trace`].
    #[must_use]
    #[inline]
    pub const fn increment_severity(&self) -> Self {
        match self {
            Self::Off => Self::Error,
            Self::Error => Self::Warn,
            Self::Warn => Self::Info,
            Self::Info => Self::Debug,
            Self::Debug | Self::Trace => Self::Trace,
        }
    }

    /// The next-lower-verbosity filter, saturating at [`LevelFilter::Off`].
    #[must_use]
    #[inline]
    pub const fn decrement_severity(&self) -> Self {
        match self {
            Self::Off | Self::Error => Self::Off,
            Self::Warn => Self::Error,
            Self::Info => Self::Warn,
            Self::Debug => Self::Info,
            Self::Trace => Self::Debug,
        }
    }
}

impl fmt::Display for LevelFilter {
    /// `fmt.pad`, as upstream (`log-0.4.32/src/lib.rs:678-681`). `aterm_log`
    /// has no `Display` for `LevelFilter` at all.
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.pad(self.as_str())
    }
}

impl FromStr for LevelFilter {
    type Err = ParseLevelError;

    /// Upstream's semantics exactly (`log-0.4.32/src/lib.rs:665-676`): ASCII
    /// case-insensitive, no trimming, and `"off"` IS accepted — the scan starts
    /// at index 0. That asymmetry with [`Level::from_str`] is deliberate
    /// upstream and is pinned in `tests/consumer_forms.rs`.
    fn from_str(level: &str) -> Result<Self, Self::Err> {
        for (idx, name) in LOG_LEVEL_NAMES.iter().enumerate() {
            if name.eq_ignore_ascii_case(level) {
                return level_filter_from_usize(idx).ok_or(ParseLevelError(()));
            }
        }
        Err(ParseLevelError(()))
    }
}

// The four cross-type comparison impls.
//
// UNUSED IN TODAY'S GRAPH — every one of the ten comparison sites found is
// same-type (`level == log::Level::Warn` ×4 in wgpu-hal's dxgi/exception.rs,
// `log::max_level() >= log::LevelFilter::X` ×4, and two more). They are here
// anyway, because `Level` and `LevelFilter` being directly comparable is the
// idiom upstream's own type documentation advertises, and a wgpu or naga bump
// that writes `if level <= log::max_level()` would otherwise fail to compile on
// a cell this machine cannot build. Reproducing them costs twenty lines and the
// semantics are fully determined by the discriminants.

impl PartialEq<LevelFilter> for Level {
    #[inline]
    fn eq(&self, other: &LevelFilter) -> bool {
        *self as usize == *other as usize
    }
}

impl PartialOrd<LevelFilter> for Level {
    #[inline]
    fn partial_cmp(&self, other: &LevelFilter) -> Option<cmp::Ordering> {
        Some((*self as usize).cmp(&(*other as usize)))
    }
}

impl PartialEq<Level> for LevelFilter {
    #[inline]
    fn eq(&self, other: &Level) -> bool {
        *self as usize == *other as usize
    }
}

impl PartialOrd<Level> for LevelFilter {
    #[inline]
    fn partial_cmp(&self, other: &Level) -> Option<cmp::Ordering> {
        Some((*self as usize).cmp(&(*other as usize)))
    }
}

/// The type returned by `from_str` when the string doesn't match any log level.
///
/// Upstream also carries `impl error::Error for ParseLevelError` behind its
/// `std` feature. This crate is `#![no_std]` unconditionally (see the top of the
/// file), so that impl is absent — the only item on this crate's surface that
/// upstream has and this does not. Nothing in the graph constructs, matches or
/// boxes one, so nothing can notice; it is recorded here rather than left for
/// someone to find.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseLevelError(());

impl fmt::Display for ParseLevelError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.write_str(LEVEL_PARSE_ERROR)
    }
}

/// Index -> [`Level`], for the `FromStr` scan. Index 0 (`"OFF"`) has no `Level`.
#[inline]
const fn level_from_usize(u: usize) -> Option<Level> {
    match u {
        1 => Some(Level::Error),
        2 => Some(Level::Warn),
        3 => Some(Level::Info),
        4 => Some(Level::Debug),
        5 => Some(Level::Trace),
        _ => None,
    }
}

/// Index -> [`LevelFilter`], for the `FromStr` scan and for [`max_level`].
#[inline]
const fn level_filter_from_usize(u: usize) -> Option<LevelFilter> {
    match u {
        0 => Some(LevelFilter::Off),
        1 => Some(LevelFilter::Error),
        2 => Some(LevelFilter::Warn),
        3 => Some(LevelFilter::Info),
        4 => Some(LevelFilter::Debug),
        5 => Some(LevelFilter::Trace),
        _ => None,
    }
}

// ===========================================================================
// THE SEAM
// ===========================================================================
//
// Everything below this line — the level global and the seven macros — is the
// ONLY place `noop` and `forward` differ. Above the line is pure data that both
// modes share unchanged. The seam is kept this narrow on purpose, so the
// owner's choice stays a reversible edit rather than a rewrite.
//
//   noop (SHIPPED)  macros expand to `{}` / `false`; `max_level()` is `Off`.
//   forward         macros expand into `aterm_log::__log(...)`; `max_level()`
//                   returns `aterm_log::max_level()` mapped through a two-line
//                   `From<aterm_log::LevelFilter>`. Requires adding the
//                   `aterm-log` dependency the manifest currently omits, and
//                   gives up `no_std` for the seven `no_std` consumers.
//
// THREE THINGS THE FORWARD DECISION MUST BE MADE WITH IN VIEW, none of which is
// visible from the diff that would make it:
//
// 1. It turns the four `max_level()` sites TRUE and switches on Vulkan
//    validation VERBOSE/INFO/WARNING severities — see reason 1 in the module
//    docs. This is a runtime cost on Windows and Linux, per frame, and it is
//    not a logging change; it changes what the driver is asked to do.
// 2. Six of the twelve `log!` sites are inside `std::panic::catch_unwind`
//    closures on a driver callback thread (wgpu-hal `auxil/dxgi/exception.rs:84`,
//    `gles/mod.rs:1105`, `vulkan/instance.rs:97,116,129,149`). Today those
//    closures have empty bodies. Forwarding makes them format and emit *inside*
//    a `catch_unwind` on the Vulkan/D3D12 debug-callback thread — a re-entrancy
//    path from a driver callback into aterm's own logger and its watchdog.
// 3. Five of the twenty consumers are reached only through
//    `[dev-dependencies]` — alacritty_terminal, termwiz, vte, wezterm-bidi,
//    wezterm-dynamic — and they contribute 147 macro sites, 92 of them
//    `trace!` (61 in alacritty_terminal alone). Where those crates serve as aterm's
//    differential ORACLES, forwarding changes what a failing oracle run prints,
//    and blanking them (today) already changes what it does not print.

/// The backing global for [`max_level`], structurally identical to upstream's
/// `MAX_LOG_LEVEL_FILTER` (`log-0.4.32/src/lib.rs:458`) — *minus the setter*.
///
/// THIS CRATE EXPORTS NO `set_max_level`, so nothing can ever store anything
/// here and the load is `0` == [`LevelFilter::Off`] for the whole process
/// lifetime. Keeping the static rather than writing `LevelFilter::Off` as a
/// literal is what makes that an invariant a reader can *check* — `grep
/// MAX_LOG_LEVEL_FILTER src/lib.rs` returns this declaration and the one load
/// below, and no store — and it is where a `forward` flip would attach.
static MAX_LOG_LEVEL_FILTER: AtomicUsize = AtomicUsize::new(0);

/// The current maximum log level — always [`LevelFilter::Off`] in this build.
///
/// # This is the one item that must not delegate, and it is the sharpest edit
/// in the crate
///
/// Four call sites read it, and they gate real work rather than formatting:
/// `wgpu-hal-29.0.3/src/vulkan/instance.rs:718,721,724` build the
/// `vk::DebugUtilsMessageSeverityFlagsEXT` bitmask handed to the Vulkan driver,
/// and `src/gles/egl.rs:424` gates an `egl.get_config_count()` /
/// `egl.get_configs()` loop. All four are false today because upstream's global
/// starts at `Off` and nothing in the graph calls `log::set_max_level`.
///
/// `aterm_log::max_level()` is a *different global*, and it is set: `Info` in
/// `crates/aterm/src/main.rs`, `Trace` in `crates/aterm-gui/src/watchdog.rs`.
/// Forwarding to it would flip all four sites on. So this reads
/// [`MAX_LOG_LEVEL_FILTER`], which nothing writes.
///
/// Upstream reaches its answer with
/// `unsafe { mem::transmute(MAX_LOG_LEVEL_FILTER.load(…)) }`, sound only because
/// the setter is the sole writer. A total match is the same function with no
/// `unsafe` and no soundness argument to maintain.
#[must_use]
pub fn max_level() -> LevelFilter {
    match level_filter_from_usize(MAX_LOG_LEVEL_FILTER.load(Ordering::Relaxed)) {
        Some(filter) => filter,
        None => LevelFilter::Off,
    }
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------
//
// Every macro below matches `($($discarded:tt)*)`. That single arm accepts any
// balanced token tree, which is the entire observed grammar and then some:
//
//   * the four upstream arms of each level macro — bare, `target:`, `logger:`,
//     and `logger: … target: …` — without enumerating any of them;
//   * the structured `key = value; "fmt"` form, which no consumer writes;
//   * raw strings with embedded `{{`/`}}` spanning ten lines (gpu-allocator);
//   * trailing commas, inline format captures, and mixed positional/inline.
//
// MATCHING TOKENS RATHER THAN A GRAMMAR IS THE POINT, not laziness: it is the
// direct expression of "we never inspect the arguments", and it is the only
// formulation that cannot be broken by a form nobody audited. It is also the
// formulation that FIXES the specific gap that made `aterm_log`'s macros
// unusable here — those splice `$($arg:tt)*` straight into `format_args!`, and
// `format_args!` REJECTS a `target:` prefix with
// `error: expected `,`, found `:``. There are eighteen `target:` sites, all in
// smithay-client-toolkit 0.19.2 (9 `warn!`, 5 `debug!`, 3 `error!`, 1 `trace!`),
// i.e. entirely on the one cell that cannot be compiled on this machine. A
// token-discarding arm swallows them; a `format_args!`-splicing arm does not.
//
// The expansion is `{}`: an empty block. It has to be an expression, because
// these macros appear in expression position as well as statement position —
// `_ => warn!("unknown keymap format 0x{:x}", value)` is a match arm whose value
// is the macro (smithay `seat/keyboard/mod.rs:566`), and wgpu-types'
// `backend.rs:800` puts one in a `map_err` closure's tail position. `{}` is a
// valid expression of type `()` and a valid statement, so one expansion covers
// both. It also evaluates nothing, which is the point.
//
// All of them are `#[macro_export]`, which puts them at the crate root. That
// makes all four spellings the consumers use work at once: `log::debug!(…)`
// fully qualified, a bare `debug!(…)` after `use log::debug;`, a bare one after
// `use log::*;` (gpu-allocator `allocator/mod.rs:8`), and a bare one after
// `#[macro_use] extern crate log;` in an edition-2015 crate (gl_generator
// `lib.rs:63-64`, a Windows build-dependency and the graph's only such site).

/// Log at the error level. Expands to `{}`; arguments are never evaluated.
///
/// ```
/// // Never called: a disabled callsite evaluates nothing, here or upstream.
/// #[allow(dead_code)]
/// fn never_called() -> u32 { unreachable!() }
///
/// log::error!("Root index {} is not bound", never_called());
/// log::error!(target: "sctk", "unknown decoration mode 0x{:x}", never_called());
/// ```
#[macro_export]
macro_rules! error {
    ($($discarded:tt)*) => {{}};
}

/// Log at the warn level. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! warn {
    ($($discarded:tt)*) => {{}};
}

/// Log at the info level. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! info {
    ($($discarded:tt)*) => {{}};
}

/// Log at the debug level. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! debug {
    ($($discarded:tt)*) => {{}};
}

/// Log at the trace level. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! trace {
    ($($discarded:tt)*) => {{}};
}

/// Log at a runtime-chosen [`Level`]. Expands to `{}`; arguments are never
/// evaluated — **including the level expression**.
///
/// That last clause is this crate's only behavioural delta from upstream, which
/// binds `let lvl = $lvl;` before its enabled-check and so evaluates the level
/// even on a disabled callsite. All twelve `log!` sites in the graph pass a
/// plain `Copy` local or closure parameter with no side effect (enumerated in
/// the module docs), so the delta is unobservable today. It is pinned by a test
/// rather than left to be rediscovered.
///
/// ```
/// use log::{log, Level};
///
/// let level = Level::Warn;
/// assert_eq!(level, Level::Warn);
/// log!(level, "EGL '{}' code 0x{:x}", "eglInitialize", 0x3001);
/// ```
#[macro_export]
macro_rules! log {
    ($($discarded:tt)*) => {{}};
}

/// Whether a callsite at the given [`Level`] would emit. Always `false`.
///
/// It must be an EXPRESSION, not a statement: the sole call site in the graph
/// is `if log::log_enabled!(log::Level::Debug) {` (naga
/// `src/proc/overloads/list.rs:98`), and naga uses it to skip building an
/// expensive `for_debug` rendering.
///
/// `false` is not an approximation. Upstream expands to
/// `lvl <= STATIC_MAX_LEVEL && lvl <= max_level() && enabled(…)`, and with
/// [`max_level`] at [`LevelFilter::Off`] the second conjunct is
/// `Level::Error as usize (1) <= LevelFilter::Off as usize (0)`, which is false
/// for all five levels. The same delta as [`log!`] applies: `$lvl` is not read.
///
/// ```
/// assert!(!log::log_enabled!(log::Level::Debug));
/// ```
#[macro_export]
macro_rules! log_enabled {
    ($($discarded:tt)*) => {
        false
    };
}
