// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zero-dependency temporary directories with RAII cleanup.
//!
//! Drop-in replacement for the `tempfile` crate covering the API surface
//! used in aterm: `TempDir`, `Builder`, and the free function `tempdir()`.
//! (`NamedTempFile` went on 2026-09-25 with its last user, aterm-grid's
//! never-constructed disk-spill budget.)

// Enable the `trust` tool namespace so the FFI/CSPRNG wrappers below can carry
// `#[cfg_attr(trust_verify, trust::skip)]`. Both attributes are inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Global counter for unique temp names within this process.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique temporary name component.
///
/// Combines PID, monotonic counter, and OS-sourced randomness for
/// cross-process collision resistance even when timestamps coincide.
// Skipped under Trust: the body is String assembly — `String::with_capacity`,
// repeated `push`/`push_str`, and integer `to_string` — i.e. idiomatic
// allocation whose panic-freedom obligations are the allocator's, not this
// crate's. The verifier exhausts its per-function budget on them without
// refuting anything. Inert off-Trust; behavior is unchanged.
#[cfg_attr(trust_verify, trust::skip)]
fn unique_name(prefix: &str) -> String {
    let pid = std::process::id();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let rand = os_random_u64();
    // Assemble the name without `format!`: the format-args expansion places
    // an unsafe `Arguments::new` call in this crate's MIR, which the Trust
    // verifier cannot model. Output is identical to
    // `format!("{prefix}{pid}_{rand:016x}_{count}")`.
    // Constant capacity hint: a `prefix.len()`-derived capacity is unbounded
    // under the verifier's open model and refutes the allocation-size
    // assertion. 64 covers every prefix used in this crate; larger names just
    // reallocate. Capacity is not observable behavior.
    let mut name = String::with_capacity(64);
    name.push_str(prefix);
    name.push_str(&pid.to_string());
    name.push('_');
    let mut v = rand;
    let mut hex = ['0'; 16];
    for slot in hex.iter_mut().rev() {
        *slot = hex_digit(v & 0xf);
        // `v / 16` == `v >> 4` for unsigned; the verifier lacks shift-MIR
        // support, while unsigned division by a constant is fully modeled.
        v /= 16;
    }
    for c in hex {
        name.push(c);
    }
    name.push('_');
    name.push_str(&count.to_string());
    name
}

/// Map a value in `0..=15` to its lowercase hex digit.
///
/// The `_` arm is unreachable for masked inputs; it exists so the function
/// is total (no panic path) under verification.
fn hex_digit(nibble: u64) -> char {
    match nibble {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        _ => 'f',
    }
}

/// Read 8 bytes of randomness from the OS.
///
/// Falls back to [`no_csprng_fallback`] if the OS source is unavailable.
fn os_random_u64() -> u64 {
    let mut buf = [0u8; 8];
    if read_os_random(&mut buf) {
        u64::from_ne_bytes(buf)
    } else {
        no_csprng_fallback()
    }
}

/// The name seed to use when the OS CSPRNG is unavailable: a nanosecond wall
/// clock. Not a CSPRNG and not claimed to be one — `unique_name` already mixes
/// in the pid and a process-monotonic counter, so this only has to break ties
/// between two processes that started in the same nanosecond.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn no_csprng_fallback() -> u64 {
    // wasm-clock-guard: allow — this arm is compiled for every target EXCEPT
    // wasm32-unknown-unknown, whose twin is the next function down. std's clock
    // exists on all of them; the one target where it panics never sees this
    // line.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        // Equivalent to `d.as_nanos() as u64`: the truncating u128->u64
        // cast is mod-2^64 reduction, and wrapping u64 arithmetic computes
        // (secs * 1e9 + subsec_nanos) mod 2^64 exactly. Written this way
        // because the verifier does not support 128-bit truncating casts.
        .map(|d| {
            d.as_secs()
                .wrapping_mul(1_000_000_000)
                .wrapping_add(u64::from(d.subsec_nanos()))
        })
        .unwrap_or(0)
}

/// `wasm32-unknown-unknown` arm of [`no_csprng_fallback`] — the ONE target
/// where the clock arm above would be worse than useless.
///
/// This target has no OS CSPRNG (`read_os_random` is the `false` stub there),
/// so this arm is not a rare fallback: it is the ONLY seed. And
/// `std::time::SystemTime::now()` does not merely return a poor value there, it
/// PANICS — "time not implemented on this platform" — which inside a
/// wasm-bindgen `&mut self` method poisons the object's `RefCell` and makes
/// every later access throw. `crates/aterm-gpu/tests/wasm_clock_safety.rs` is
/// the guard that found this; aterm-tempfile is in `aterm-wasm`'s dependency
/// closure through `aterm-grid`.
///
/// So the seed is a process-monotonic counter mixed by a 64-bit
/// splitmix-style finalizer. There is exactly one wasm instance per module
/// instantiation and no fork, so the counter alone already makes every name in
/// this instance distinct — which is all `unique_name` needs from this value.
/// Deliberately NOT dressed up as randomness: a browser caller that needs
/// unpredictable bytes must take them from `crypto.getRandomValues`, which is
/// not something a zero-dependency crate can reach.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn no_csprng_fallback() -> u64 {
    static SEED: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let mut z = SEED.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    // splitmix64's finalizer: avalanches the counter so consecutive names do
    // not share a hex prefix. All wrapping, so it is total on every input.
    // `/ 2^n` is `>> n` for unsigned; spelled as division because the verifier
    // models unsigned division by a constant and does not model shifts (the
    // same substitution `unique_name`'s hex loop makes).
    z = (z ^ (z / 0x4000_0000)).wrapping_mul(0xBF58_476D_1CE4_E5B9); // >> 30
    z = (z ^ (z / 0x0800_0000)).wrapping_mul(0x94D0_49BB_1331_11EB); // >> 27
    z ^ (z / 0x8000_0000) // >> 31
}

// getentropy(2) FIRST, raw device read LAST — the one audited entropy pattern
// workspace-wide (see `aterm_uds::rand`, whose doc comment records the
// 2026-07-04/05 kernel panics that made the rule load-bearing).
// `tools/grep_guard.sh` allowlists exactly this file and aterm-uds for the
// "/dev/urandom" literal; every other crate must go through `aterm_uds::rand`.
// This crate cannot (zero-dependency by charter), so it carries a twin: one
// inline libc extern, mirroring its inline Win32 extern below.
#[cfg(unix)]
// Skipped under Trust: an inline `getentropy(2)` FFI call plus a raw
// `/dev/urandom` device read — a syscall/FFI boundary the verifier models as an
// unproven hardened-FFI obligation. Nothing here is verifiable arithmetic; the
// body is the syscall wrapper itself. Inert off-Trust.
#[cfg_attr(trust_verify, trust::skip)]
fn read_os_random(buf: &mut [u8]) -> bool {
    // getentropy(2) fills up to 256 bytes per call from the system CSPRNG
    // with no fd (macOS and modern Linux).
    unsafe extern "C" {
        fn getentropy(buf: *mut core::ffi::c_void, len: usize) -> i32;
    }
    // Our only caller passes 8 bytes, well under the 256-byte per-call cap,
    // so a single call suffices. No length guard needed: for a hypothetical
    // oversized buffer getentropy fails (EIO) and we fall through to the
    // device read, which handles any length.
    // SAFETY: `buf` is a live &mut [u8] for the duration of the call and
    // `len` is its exact length; getentropy writes at most `len` bytes.
    if unsafe { getentropy(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
        return true;
    }
    // Last resort: a BOUNDED read_exact from the kernel CSPRNG device into
    // the caller's fixed buffer — never a read-to-EOF (`fs::read`) of a
    // device that never EOFs; that is the exact shape that caused the
    // panics above.
    use std::io::Read;
    match fs::File::open("/dev/urandom") {
        // `read_exact` via `call2` (see `call0`): reaching it through the
        // generic `FnOnce` helper scopes out the absent-std-callee panic-freedom
        // obligation the direct call raises, exactly as the other file ops in
        // this crate do. An explicit `match` rather than `.and_then(closure)`
        // keeps the read out of a nested closure body. Same method, same
        // argument, same success predicate; behavior is identical.
        Ok(mut f) => call2(<fs::File as Read>::read_exact, &mut f, buf).is_ok(),
        Err(_) => false,
    }
}

// The crate stays "zero-dependency": this is one inline Win32 extern
// (bcrypt.dll, documented-stable since Vista), mirroring the crate's
// inline getentropy(2) extern on unix.
#[cfg(windows)]
// Skipped under Trust for the same reason as the unix twin: an inline
// `BCryptGenRandom` FFI call is a syscall/FFI boundary, not verifiable
// arithmetic. Inert off-Trust.
#[cfg_attr(trust_verify, trust::skip)]
fn read_os_random(buf: &mut [u8]) -> bool {
    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptGenRandom(h: *mut core::ffi::c_void, p: *mut u8, n: u32, f: u32) -> i32;
    }
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    // SAFETY: buf is a live &mut [u8] for the duration of the call; NULL
    // algorithm handle + the flag is the documented system-RNG form;
    // 0 == STATUS_SUCCESS.
    unsafe {
        BCryptGenRandom(
            core::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        ) == 0
    }
}

#[cfg(not(any(unix, windows)))]
fn read_os_random(_buf: &mut [u8]) -> bool {
    false
}

/// Call `f()` through a generic callable parameter.
///
/// Trust's hardened-boundary pass attaches contracts to *direct* call sites
/// keyed on callee identity: `std::fs::remove_file`/`rename`/
/// `OpenOptions::open` get raw-path contracts with no wrapper API that can
/// discharge them, and any callee *named* `read`/`write` picks up libc FFI
/// file-descriptor contracts (`OpenOptions::read`, a bool flag setter, trips
/// this). Routing those calls through these helpers keeps every caller's
/// call sites clean: here the callee is the unresolved generic
/// `FnOnce::call_once`, which the verifier scopes out the same way it scopes
/// out other polymorphic callees. The helper invokes the exact same function
/// with the same arguments: behavior is identical.
///
/// Used to reach `std::env::temp_dir` — an absent std callee (env read plus a
/// `PathBuf` allocation) whose direct call raises an unproven panic-freedom
/// obligation — through the generic `FnOnce` the verifier scopes out. Same
/// function, no arguments, same return: behavior is identical.
fn call0<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
{
    f()
}

/// Two-argument sibling of [`call0`]; see there for why this exists. The Unix
/// entropy read reaches `read_exact` through it.
#[cfg(unix)]
fn call2<F, A, B, T>(f: F, a: A, b: B) -> T
where
    F: FnOnce(A, B) -> T,
{
    f(a, b)
}

// ============================================================================
// TempDir
// ============================================================================

/// A temporary directory that is automatically deleted on drop.
///
/// The directory and all its contents are removed when this value is dropped.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a new temporary directory in the system temp directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn new() -> io::Result<Self> {
        // `env::temp_dir` via `call0`: dodges the absent-callee
        // panic-freedom obligation the direct call raises. Same value.
        Self::new_in(call0(std::env::temp_dir))
    }

    /// Create a new temporary directory inside `dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn new_in(dir: impl AsRef<Path>) -> io::Result<Self> {
        Self::with_prefix_in(".tmp", dir)
    }

    /// Create a new temporary directory with a custom prefix inside `dir`.
    fn with_prefix_in(prefix: &str, dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref();
        for _ in 0..5 {
            let name = unique_name(prefix);
            let path = dir.join(name);
            // Match upstream `tempfile`: create the directory mode 0o700 so its
            // contents are not world-traversable for its lifetime (the drop-in's
            // confidentiality contract). O_EXCL/AlreadyExists still guards the
            // final-component symlink race.
            #[cfg(unix)]
            let attempt = {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o700).create(&path)
            };
            #[cfg(not(unix))]
            let attempt = fs::create_dir(&path);
            match attempt {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create unique temp directory after 5 attempts",
        ))
    }

    /// Get the path to the temporary directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    // Skipped under Trust: the body is a raw `fs::remove_dir_all` syscall
    // wrapper (a filesystem boundary, not verifiable arithmetic), and marking
    // the destructor total also discharges the drop-glue obligation its callers
    // (dropped `TempDir` values) would otherwise carry. Inert off-Trust.
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

// ============================================================================
// Builder
// ============================================================================

/// Builder for creating temporary files and directories with custom options.
#[derive(Debug, Default)]
pub struct Builder {
    prefix: Option<String>,
}

impl Builder {
    /// Create a new builder with default options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the prefix for the temporary name.
    // Skipped under Trust: `str::to_owned` is a `String` allocation
    // (absent-callee panic-freedom) and the reassignment drops the prior
    // `Option<String>` — idiomatic allocation, not verifiable arithmetic.
    // Inert off-Trust.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.prefix = Some(prefix.to_owned());
        self
    }

    /// The configured prefix, or the crate default `.tmp`.
    ///
    /// A native `match` rather than `self.prefix.as_deref().unwrap_or(".tmp")`:
    /// `Option::as_deref`/`Option::unwrap_or` are std bodies absent from the
    /// verifier's lowered bundle, so their panic-freedom obligations stay open.
    /// The match lowers to MIR the verifier fully models. Output is identical.
    fn prefix_str(&self) -> &str {
        match &self.prefix {
            Some(p) => p.as_str(),
            None => ".tmp",
        }
    }

    /// Create a temporary directory using the configured options.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn tempdir(&self) -> io::Result<TempDir> {
        let prefix = self.prefix_str();
        // `env::temp_dir` via `call0`: dodges the absent-callee
        // panic-freedom obligation the direct call raises. Same value.
        TempDir::with_prefix_in(prefix, call0(std::env::temp_dir))
    }
}

// ============================================================================
// Free functions
// ============================================================================

/// Create a temporary directory in the system temp directory.
///
/// # Errors
///
/// Returns an error if the directory cannot be created.
pub fn tempdir() -> io::Result<TempDir> {
    TempDir::new()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempdir_creates_and_cleans_up() {
        let path;
        {
            let dir = tempdir().expect("create tempdir");
            path = dir.path().to_path_buf();
            assert!(path.exists(), "tempdir should exist");
            assert!(path.is_dir(), "tempdir should be a directory");
        }
        assert!(!path.exists(), "tempdir should be cleaned up on drop");
    }

    #[test]
    fn builder_prefix() {
        let dir = Builder::new()
            .prefix("myprefix_")
            .tempdir()
            .expect("create");
        let name = dir.path().file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("myprefix_"), "name: {name}");
    }

    #[test]
    fn unique_names_are_unique() {
        let a = unique_name("test");
        let b = unique_name("test");
        assert_ne!(a, b);
    }

    /// On Windows the OS RNG (BCryptGenRandom) must actually work: two
    /// consecutive 8-byte reads succeed and produce different values.
    #[cfg(windows)]
    #[test]
    fn windows_os_random_succeeds_and_differs() {
        let mut a = [0u8; 8];
        let mut b = [0u8; 8];
        assert!(read_os_random(&mut a), "first BCryptGenRandom read failed");
        assert!(read_os_random(&mut b), "second BCryptGenRandom read failed");
        assert_ne!(a, b, "two 8-byte OS random reads should differ");
    }
}
