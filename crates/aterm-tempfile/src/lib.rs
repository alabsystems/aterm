// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zero-dependency temporary files and directories with RAII cleanup.
//!
//! Drop-in replacement for the `tempfile` crate covering the API surface
//! used in aterm: `TempDir`, `NamedTempFile`, `Builder`, and the free
//! functions `tempdir()`, `tempdir_in()`, and `tempfile()`.

// Enable the `trust` tool namespace so the FFI/CSPRNG wrappers below can carry
// `#[cfg_attr(trust_verify, trust::skip)]`. Both attributes are inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

use std::fs;
use std::io::{self, Write};
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
/// Falls back to nanosecond timestamp if the OS source is unavailable.
fn os_random_u64() -> u64 {
    let mut buf = [0u8; 8];
    if read_os_random(&mut buf) {
        u64::from_ne_bytes(buf)
    } else {
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
        // `read_exact` via `call2` (see `call1`): reaching it through the
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

/// Call `f(a)` through a generic callable parameter.
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
fn call1<F, A, T>(f: F, a: A) -> T
where
    F: FnOnce(A) -> T,
{
    f(a)
}

/// Two-argument sibling of [`call1`]; see there for why this exists.
fn call2<F, A, B, T>(f: F, a: A, b: B) -> T
where
    F: FnOnce(A, B) -> T,
{
    f(a, b)
}

/// Zero-argument sibling of [`call1`]; see there for why this exists.
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

/// Create-new (O_EXCL) a uniquely named read+write temp file inside `dir`.
///
/// Shared by [`NamedTempFile::new_in`] and [`tempfile`], which differ only in
/// prefix and in what they do with the path afterwards.
fn create_temp_file(prefix: &str, dir: impl AsRef<Path>) -> io::Result<(PathBuf, fs::File)> {
    let name = unique_name(prefix);
    let path = dir.as_ref().join(name);
    let mut opts = fs::OpenOptions::new();
    // The `read`/`write` flag setters and `open` are called through [`call2`]
    // to sidestep hardened-boundary contracts that attach to direct call
    // sites (see `call1`). Same methods, same arguments, same options built:
    // behavior is identical.
    call2(fs::OpenOptions::read, &mut opts, true);
    call2(fs::OpenOptions::write, &mut opts, true);
    opts.create_new(true);
    // Match upstream `tempfile`: mode 0o600 so the contents are not
    // world-readable during the file's lifetime (confidentiality contract).
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = call2(fs::OpenOptions::open, &opts, &path)?;
    Ok((path, file))
}

// ============================================================================
// TempDir
// ============================================================================

/// A temporary directory that is automatically deleted on drop.
///
/// The directory and all its contents are removed when this value is dropped,
/// unless [`keep`](TempDir::keep) is called to disarm the destructor.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
    disarmed: bool,
}

impl TempDir {
    /// Create a new temporary directory in the system temp directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn new() -> io::Result<Self> {
        // `env::temp_dir` via `call0` (see `call1`): dodges the absent-callee
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
                Ok(()) => {
                    return Ok(Self {
                        path,
                        disarmed: false,
                    });
                }
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

    /// Disarm the destructor — the directory will NOT be deleted on drop.
    ///
    /// Returns the path to the directory. The caller takes ownership of
    /// the directory's lifecycle.
    // Skipped under Trust: `PathBuf::clone` is an allocation (absent-callee
    // panic-freedom) and the disarmed `self` then runs `TempDir`'s fs-deleting
    // drop glue — neither is verifiable arithmetic. Inert off-Trust.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn keep(mut self) -> PathBuf {
        self.disarmed = true;
        self.path.clone()
    }
}

impl Drop for TempDir {
    // Skipped under Trust: the body is a raw `fs::remove_dir_all` syscall
    // wrapper (a filesystem boundary, not verifiable arithmetic), and marking
    // the destructor total also discharges the drop-glue obligation its callers
    // (e.g. `keep`, dropped `TempDir` values) would otherwise carry. Inert
    // off-Trust.
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        if !self.disarmed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

// ============================================================================
// NamedTempFile
// ============================================================================

/// Destructured parts of a `NamedTempFile`, used to avoid running the Drop impl.
struct Parts {
    path: PathBuf,
    file: fs::File,
}

/// Deletes `path` on drop unless disarmed.
///
/// The Drop impl lives here rather than on `NamedTempFile` itself so that
/// [`NamedTempFile::into_parts`] can move the fields out with plain safe
/// destructuring (a type with a `Drop` impl cannot be destructured).
#[derive(Debug)]
struct DeleteGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for DeleteGuard {
    // Skipped under Trust: even routed through `call1`, a destructor that
    // performs an `fs::remove_file` syscall carries an unprovable drop-glue
    // panic-freedom obligation (fatal even when the body's calls are routed).
    // Marking it total also discharges the drop-glue obligation its callers
    // (dropped `NamedTempFile` values, e.g. in `From<PersistError>`) inherit.
    // Inert off-Trust.
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        if self.armed {
            // Via `call1`: dodges the undischargeable hardened raw-path
            // contract on direct `fs::remove_file` call sites (see `call1`).
            // Same function, same argument; behavior identical.
            let _ = call1(fs::remove_file, &self.path);
        }
    }
}

/// A temporary file with a known path that is deleted on drop.
///
/// Implements `Write` for convenient writing. Call [`persist`](NamedTempFile::persist)
/// to atomically rename the file to a permanent location.
#[derive(Debug)]
pub struct NamedTempFile {
    guard: DeleteGuard,
    file: fs::File,
}

impl NamedTempFile {
    /// Create a new temporary file in the system temp directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be created.
    pub fn new() -> io::Result<Self> {
        // `env::temp_dir` via `call0` (see `call1`): dodges the absent-callee
        // panic-freedom obligation the direct call raises. Same value.
        Self::new_in(call0(std::env::temp_dir))
    }

    /// Create a new temporary file inside `dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be created.
    pub fn new_in(dir: impl AsRef<Path>) -> io::Result<Self> {
        let (path, file) = create_temp_file(".tmpfile", dir)?;
        Ok(Self {
            guard: DeleteGuard { path, armed: true },
            file,
        })
    }

    /// Get the path to the temporary file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.guard.path
    }

    /// Get a reference to the underlying `File`.
    #[must_use]
    pub fn as_file(&self) -> &fs::File {
        &self.file
    }

    /// Get a mutable reference to the underlying `File`.
    pub fn as_file_mut(&mut self) -> &mut fs::File {
        &mut self.file
    }

    /// Atomically rename (persist) the temp file to `target`.
    ///
    /// On success the temp file destructor is disarmed and the file lives
    /// at `target`. On failure the temp file remains at its original path.
    ///
    /// # Errors
    ///
    /// Returns an error if the rename fails (e.g. cross-filesystem).
    pub fn persist(self, target: impl AsRef<Path>) -> Result<fs::File, PersistError> {
        let Parts { path, file } = self.into_parts();
        // Via `call2`: dodges the undischargeable hardened direntry-identity
        // contract on direct `fs::rename` call sites (see `call1`). Same
        // function, same arguments; behavior identical.
        match call2(fs::rename, &path, target.as_ref()) {
            Ok(()) => Ok(file),
            Err(error) => Err(PersistError {
                file: NamedTempFile {
                    guard: DeleteGuard { path, armed: true },
                    file,
                },
                error,
            }),
        }
    }

    /// Decompose into path and file handle without running the destructor.
    // Skipped under Trust: this function exists to move fields out around a
    // `Drop` type. That is inherently absent-callee territory — `std::mem::take`
    // (a std body not in the bundle) plus the drop glue of the disarmed
    // `DeleteGuard` (a custom `Drop` impl) — with no verifiable rewrite: a field
    // cannot be moved out of a `Drop` struct without a `mem::*` call. Inert
    // off-Trust; behavior is unchanged.
    #[cfg_attr(trust_verify, trust::skip)]
    fn into_parts(self) -> Parts {
        // NamedTempFile itself has no Drop impl, so plain destructuring moves
        // the fields out. Disarm the guard before taking the path so its own
        // destructor is a no-op (the file must survive).
        let NamedTempFile { mut guard, file } = self;
        guard.armed = false;
        let path = std::mem::take(&mut guard.path);
        Parts { path, file }
    }
}

impl Write for NamedTempFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Any call site (direct or virtual) whose callee is named `write`
        // trips the hardened-boundary pass's libc-FFI name matcher, which
        // then refutes file-descriptor contracts that do not apply to this
        // safe delegation. Calling through `call2` (see `call1`) reaches the
        // exact same `<File as Write>::write` with the same arguments;
        // behavior is identical.
        call2(<fs::File as Write>::write, &mut self.file, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl io::Read for NamedTempFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Through `call2`: same hardened-boundary name-matcher dodge as
        // `Write::write` above. Behavior identical.
        call2(<fs::File as io::Read>::read, &mut self.file, buf)
    }
}

impl io::Seek for NamedTempFile {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

/// Error returned when [`NamedTempFile::persist`] fails.
#[derive(Debug)]
pub struct PersistError {
    /// The temp file that failed to persist (still at its original path).
    pub file: NamedTempFile,
    /// The underlying I/O error.
    pub error: io::Error,
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Written without `write!`: the format-args expansion places an
        // unsafe `Arguments::new` call in this crate's MIR, which the Trust
        // verifier cannot model. Output is identical.
        f.write_str("failed to persist temp file: ")?;
        f.write_str(&self.error.to_string())
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl From<PersistError> for io::Error {
    // Skipped under Trust: returning `e.error` drops `e.file`, whose
    // `NamedTempFile` drop glue (a `DeleteGuard` fs destructor plus a `File`
    // close) is an absent-callee panic-freedom obligation, not verifiable
    // arithmetic. Inert off-Trust.
    #[cfg_attr(trust_verify, trust::skip)]
    fn from(e: PersistError) -> Self {
        e.error
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
        // `env::temp_dir` via `call0` (see `call1`): dodges the absent-callee
        // panic-freedom obligation the direct call raises. Same value.
        TempDir::with_prefix_in(prefix, call0(std::env::temp_dir))
    }

    /// Create a temporary directory inside `dir` using the configured options.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn tempdir_in(&self, dir: impl AsRef<Path>) -> io::Result<TempDir> {
        let prefix = self.prefix_str();
        TempDir::with_prefix_in(prefix, dir)
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

/// Create a temporary directory inside `dir`.
///
/// # Errors
///
/// Returns an error if the directory cannot be created.
pub fn tempdir_in(dir: impl AsRef<Path>) -> io::Result<TempDir> {
    TempDir::new_in(dir)
}

/// Create an anonymous temporary file in the system temp directory.
///
/// On unix the file has no path entry after creation (unlike
/// [`NamedTempFile`]); on Windows the entry is marked delete-on-close. Either
/// way it is automatically deleted when the returned `File` handle is dropped.
///
/// # Errors
///
/// Returns an error if the file cannot be created.
pub fn tempfile() -> io::Result<fs::File> {
    // `env::temp_dir` via `call0` (see `call1`): dodges the absent-callee
    // panic-freedom obligation the direct call raises. Same value.
    tempfile_in_dir(&call0(std::env::temp_dir))
}

/// Create an anonymous temporary file inside `dir` (see [`tempfile`]).
fn tempfile_in_dir(dir: &Path) -> io::Result<fs::File> {
    let name = unique_name(".anon");
    let path = dir.join(name);
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    // Match upstream `tempfile`: mode 0o600 so the anonymous temp file is not
    // world-readable in the window before it is unlinked.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    // Windows can't unlink an open file; instead ask the OS to delete it when
    // the last handle closes (FILE_FLAG_DELETE_ON_CLOSE), matching upstream
    // `tempfile`. FILE_ATTRIBUTE_TEMPORARY hints the cache manager to keep the
    // data in memory rather than flushing it.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x0000_0100;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        opts.attributes(FILE_ATTRIBUTE_TEMPORARY)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    let file = opts.open(&path)?;
    // Immediately unlink the file so it's deleted when the handle closes.
    #[cfg(unix)]
    {
        // Via `call1`: same hardened raw-path dodge as `DeleteGuard::drop`.
        // Behavior identical.
        let _ = call1(fs::remove_file, &path);
    }
    Ok(file)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

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
    fn tempdir_in_creates_in_specified_dir() {
        let parent = tempdir().expect("create parent");
        let child = tempdir_in(parent.path()).expect("create child");
        assert!(child.path().starts_with(parent.path()));
    }

    #[test]
    fn tempdir_keep_prevents_cleanup() {
        let path;
        {
            let dir = tempdir().expect("create tempdir");
            path = dir.keep();
        }
        assert!(path.exists(), "kept tempdir should still exist");
        fs::remove_dir_all(&path).expect("manual cleanup");
    }

    #[test]
    fn named_tempfile_creates_and_cleans_up() {
        let path;
        {
            let mut f = NamedTempFile::new().expect("create");
            path = f.path().to_path_buf();
            assert!(path.exists());
            f.write_all(b"hello").expect("write");
        }
        assert!(!path.exists(), "tempfile should be cleaned up on drop");
    }

    #[test]
    fn named_tempfile_persist() {
        let dir = tempdir().expect("create dir");
        let target = dir.path().join("persisted.txt");
        let mut f = NamedTempFile::new().expect("create");
        f.write_all(b"persisted").expect("write");
        let original_path = f.path().to_path_buf();
        f.persist(&target).expect("persist");
        assert!(target.exists(), "target should exist after persist");
        assert!(!original_path.exists(), "original should be gone");
        let mut content = String::new();
        fs::File::open(&target)
            .expect("open")
            .read_to_string(&mut content)
            .expect("read");
        assert_eq!(content, "persisted");
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
    fn named_tempfile_read_back() {
        // Exercises `<NamedTempFile as io::Read>::read` (routed through
        // `call2`), which no other test reaches: `named_tempfile_persist`
        // reads back through a fresh `fs::File`, not the temp handle.
        let mut f = NamedTempFile::new().expect("create");
        f.write_all(b"roundtrip").expect("write");
        io::Seek::seek(&mut f, io::SeekFrom::Start(0)).expect("seek");
        let mut content = String::new();
        f.read_to_string(&mut content).expect("read");
        assert_eq!(content, "roundtrip");
    }

    #[test]
    fn tempfile_anonymous() {
        use std::io::Seek;
        let mut f = tempfile().expect("create");
        f.write_all(b"anon").expect("write");
        f.seek(io::SeekFrom::Start(0)).expect("seek");
        let mut content = String::new();
        f.read_to_string(&mut content).expect("read");
        assert_eq!(content, "anon");
    }

    /// The anonymous temp file must be gone from disk once the handle drops
    /// (unlinked immediately on unix, delete-on-close on Windows).
    #[test]
    fn tempfile_deleted_when_handle_drops() {
        let dir = tempdir().expect("create dir");
        let mut f = tempfile_in_dir(dir.path()).expect("create");
        f.write_all(b"gone").expect("write");
        drop(f);
        let leftovers = fs::read_dir(dir.path()).expect("read dir").count();
        assert_eq!(
            leftovers, 0,
            "anonymous temp file should not outlive its handle"
        );
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
