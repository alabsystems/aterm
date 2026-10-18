// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zero-dependency temporary directories with RAII cleanup.
//!
//! A drop-in for the parts of the `tempfile` crate aterm uses: `TempDir`,
//! `Builder`, and the free function `tempdir()`. Tests that run a stand-in
//! script also get `stand_in` (test and debug builds).

// Register the `trust` tool namespace so the FFI/CSPRNG wrappers below can
// carry `#[cfg_attr(trust_verify, trust::skip)]`; inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a temporary name component from the pid, a process-wide counter
/// and OS randomness. The counter separates names in this process; callers
/// retry `AlreadyExists` to handle a collision with another process's name.
// Skipped under Trust: the body is `String` assembly, whose panic-freedom
// obligations are the allocator's; the verifier exhausts its budget on them.
#[cfg_attr(trust_verify, trust::skip)]
fn unique_name(prefix: &str) -> String {
    let pid = std::process::id();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let rand = os_random_u64();
    // Assembled without `format!`, whose expansion puts an unsafe
    // `Arguments::new` call in this crate's MIR that the verifier cannot
    // model. Output equals `format!("{prefix}{pid}_{rand:016x}_{count}")`.
    // The capacity is a constant because a `prefix.len()`-derived one is
    // unbounded under the verifier's model; longer names just reallocate.
    let mut name = String::with_capacity(64);
    name.push_str(prefix);
    name.push_str(&pid.to_string());
    name.push('_');
    let mut v = rand;
    let mut hex = ['0'; 16];
    for slot in hex.iter_mut().rev() {
        *slot = hex_digit(v & 0xf);
        // `v / 16` == `v >> 4` for unsigned; the verifier models unsigned
        // division by a constant but not shifts.
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
/// clock. Not a CSPRNG; `unique_name` also mixes in the pid and a counter, so
/// this only breaks ties between processes started in the same nanosecond.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn no_csprng_fallback() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        // Equals `d.as_nanos() as u64` (the truncating cast is mod 2^64, as is
        // the wrapping arithmetic); the verifier does not support 128-bit
        // truncating casts.
        .map(|d| {
            d.as_secs()
                .wrapping_mul(1_000_000_000)
                .wrapping_add(u64::from(d.subsec_nanos()))
        })
        .unwrap_or(0)
}

/// `wasm32-unknown-unknown` arm of [`no_csprng_fallback`], the one target
/// where the clock arm above cannot be used.
///
/// This target has no OS CSPRNG, so this is the only seed, and
/// `std::time::SystemTime::now()` panics there ("time not implemented on this
/// platform"). aterm-tempfile reaches `aterm-wasm` through `aterm-grid`, and
/// the browser cells' lint (`crates/aterm-time/wasm32/clippy.toml`) refuses
/// std's clock there.
///
/// The seed is a process-wide counter mixed by a splitmix-style finalizer.
/// With one wasm instance per module and no fork, the counter alone keeps
/// every name distinct, which is all `unique_name` needs. It is not
/// randomness: a browser caller needing unpredictable bytes must use
/// `crypto.getRandomValues`.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn no_csprng_fallback() -> u64 {
    static SEED: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let mut z = SEED.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    // splitmix64's finalizer: avalanches the counter so consecutive names do
    // not share a hex prefix. All wrapping, so total on every input. `/ 2^n`
    // is `>> n` for unsigned, spelled as division because the verifier does
    // not model shifts.
    z = (z ^ (z / 0x4000_0000)).wrapping_mul(0xBF58_476D_1CE4_E5B9); // >> 30
    z = (z ^ (z / 0x0800_0000)).wrapping_mul(0x94D0_49BB_1331_11EB); // >> 27
    z ^ (z / 0x8000_0000) // >> 31
}

// getentropy(2) first, a bounded device read last: the workspace's one
// entropy pattern (see `aterm_uds::rand`). This crate is zero-dependency, so
// it carries its own twin with one inline libc extern.
#[cfg(unix)]
// Skipped under Trust: an inline `getentropy(2)` FFI call plus a raw device
// read, a syscall boundary with nothing verifiable in it.
#[cfg_attr(trust_verify, trust::skip)]
fn read_os_random(buf: &mut [u8]) -> bool {
    // getentropy(2) fills up to 256 bytes per call from the system CSPRNG
    // with no fd (macOS and modern Linux).
    unsafe extern "C" {
        fn getentropy(buf: *mut core::ffi::c_void, len: usize) -> i32;
    }
    // The only caller passes 8 bytes, under the 256-byte per-call cap, so one
    // call suffices; an oversized buffer would fail (EIO) and fall through to
    // the device read, which handles any length.
    // SAFETY: `buf` is a live &mut [u8] for the duration of the call and
    // `len` is its exact length; getentropy writes at most `len` bytes.
    if unsafe { getentropy(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
        return true;
    }
    // Last resort: a bounded `read_exact` from the kernel CSPRNG device into
    // the caller's fixed buffer, never a read-to-EOF of a device that never
    // reaches EOF.
    use std::io::Read;
    match fs::File::open("/dev/urandom") {
        // `read_exact` via `call2` (see `call0`), and an explicit `match`
        // rather than a closure, so the verifier scopes out the absent std
        // callee as it does for this crate's other file ops.
        Ok(mut f) => call2(<fs::File as Read>::read_exact, &mut f, buf).is_ok(),
        Err(_) => false,
    }
}

// One inline Win32 extern (bcrypt.dll), mirroring the unix getentropy(2)
// extern, so the crate stays zero-dependency.
#[cfg(windows)]
// Skipped under Trust, as the unix twin: an FFI call, nothing verifiable.
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
/// Trust's hardened-boundary pass attaches contracts to direct call sites by
/// callee identity: `std::fs::remove_file`/`rename`/`OpenOptions::open` get
/// raw-path contracts no wrapper can discharge, and any callee named
/// `read`/`write` picks up libc file-descriptor contracts. Through this helper
/// the callee is the generic `FnOnce::call_once`, which the verifier scopes
/// out like other polymorphic callees. Behavior is identical.
///
/// Used to reach `std::env::temp_dir`, whose direct call raises an unproven
/// panic-freedom obligation.
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

/// A temporary directory, removed with all its contents on drop.
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
        // `env::temp_dir` via `call0` (see there).
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
            // A prefix contributes to one complete file name, never to its
            // parent path. Compare the raw name too, since components alone
            // would normalize separators or a leading "./" away.
            let mut components = Path::new(&name).components();
            if !matches!(components.next(), Some(std::path::Component::Normal(part)) if part == name.as_str())
                || components.next().is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a temporary directory prefix must not contain a path",
                ));
            }
            let path = dir.join(name);
            // Mode 0o700, as upstream `tempfile`, so the contents are not
            // world-traversable; O_EXCL/AlreadyExists still guards the
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

    /// The directory's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the directory and its contents after this guard is consumed. The caller
    /// takes responsibility for removing the returned path.
    #[must_use]
    pub fn keep(mut self) -> PathBuf {
        std::mem::take(&mut self.path)
    }
}

impl Drop for TempDir {
    // Skipped under Trust: a `fs::remove_dir_all` syscall wrapper; marking the
    // destructor total also discharges the drop-glue obligation of every
    // dropped `TempDir`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// Builder for temporary directories with a custom prefix.
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

    /// Set the prefix for the temporary name. Path separators and Windows
    /// drive prefixes are refused when the directory is created.
    // Skipped under Trust: `str::to_owned` allocates and the reassignment
    // drops the prior `Option<String>`; nothing verifiable.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.prefix = Some(prefix.to_owned());
        self
    }

    /// The configured prefix, or the crate default `.tmp`.
    ///
    /// A native `match` rather than `as_deref().unwrap_or(".tmp")`, whose std
    /// bodies the verifier's lowered bundle lacks.
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
        // `env::temp_dir` via `call0` (see there).
        TempDir::with_prefix_in(prefix, call0(std::env::temp_dir))
    }

    /// Create a temporary directory inside `dir` using the configured options
    /// (e.g. a short `/tmp` for a socket path, or a target-local scratch root).
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn tempdir_in(&self, dir: impl AsRef<Path>) -> io::Result<TempDir> {
        TempDir::with_prefix_in(self.prefix_str(), dir)
    }
}

/// Create a temporary directory in the system temp directory.
///
/// # Errors
///
/// Returns an error if the directory cannot be created.
pub fn tempdir() -> io::Result<TempDir> {
    TempDir::new()
}

/// Lay an executable stand-in at `path` holding `body`, mode 0555, replacing
/// whatever is there by rename.
///
/// macOS assesses every new executable at its first exec and caches the verdict
/// per inode. With syspolicyd backed up by concurrent builds, a freshly written
/// two-line script has sat at its first exec for minutes, failing the tests that
/// waited on it. So on macOS the body is kept once per user under the temp
/// directory, named by its contents, and every later call hard-links that inode
/// into place: only the first run on a machine pays the check (measured
/// 2026-10-07: a new script's first exec 278 ms; a hard link to one already run,
/// 3 ms; a fresh copy, 121 ms).
///
/// Every caller with the same body shares one inode, hence read-only: a write
/// through `path` fails instead of changing every test's stand-in. Not for a
/// stand-in a test chmods or rewrites in place, judges by inode or link count, or
/// whose body names a per-test path (it would never be reused). Where the shared
/// copy cannot be used (another volume, a copy not owned by the owner of `path`'s
/// directory) the stand-in is written fresh.
///
/// # Errors
///
/// Returns an error if `path` has no file name or the stand-in cannot be written
/// or moved into place.
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn stand_in(path: &Path, body: &str) -> io::Result<()> {
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a stand-in path names a file",
        ));
    }
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let tmp = dir.join(unique_name(".stand-in."));
    #[cfg(target_os = "macos")]
    let shared = link_shared(&tmp, dir, body.as_bytes()).is_ok();
    #[cfg(not(target_os = "macos"))]
    let shared = false;
    if !shared {
        write_read_only(&tmp, body.as_bytes())?;
    }
    // Renaming one link of an inode over another link of it does nothing and
    // succeeds, so laying a body again where it already lies leaves `tmp`.
    let renamed = fs::rename(&tmp, path);
    let _ = fs::remove_file(&tmp);
    renamed
}

/// Hard-link the shared copy of `body` at `tmp`, laying it first when it is
/// absent or not `body`. A link is kept only when [`is_stand_in`] holds for it.
/// A link that fails, or a copy not owned by `dir`'s owner, is an error that
/// leaves the shared copy (already assessed) as it was.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
fn link_shared(tmp: &Path, dir: &Path, body: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    let cache = call0(std::env::temp_dir).join("aterm-stand-ins");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&cache)?;
    let entry = cache.join(format!("{:016x}", fnv1a(body)));
    if !is_stand_in(&entry, &cache, body) {
        let fresh = cache.join(unique_name(".new."));
        write_read_only(&fresh, body)?;
        fs::rename(&fresh, &entry).inspect_err(|_| {
            let _ = fs::remove_file(&fresh);
        })?;
    }
    fs::hard_link(&entry, tmp)?;
    if is_stand_in(tmp, dir, body) {
        Ok(())
    } else {
        let _ = fs::remove_file(tmp);
        Err(io::Error::other(
            "the shared stand-in changed as it was linked",
        ))
    }
}

/// Whether `file` is a regular file holding exactly `body`, mode 0555, owned by
/// whoever owns `dir`: what was checked is the inode itself, which only its
/// owner can change.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
fn is_stand_in(file: &Path, dir: &Path, body: &[u8]) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let (Ok(meta), Ok(owner)) = (fs::symlink_metadata(file), fs::metadata(dir)) else {
        return false;
    };
    meta.is_file()
        && meta.uid() == owner.uid()
        && meta.mode() & 0o7777 == 0o555
        && fs::read(file).is_ok_and(|read| read == body)
}

/// Create `path` (it must not exist) holding `body`, then make it mode 0555.
#[cfg(all(unix, any(test, debug_assertions)))]
fn write_read_only(path: &Path, body: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(path)?;
    let written = file
        .write_all(body)
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(0o555)));
    drop(file);
    written.inspect_err(|_| {
        let _ = fs::remove_file(path);
    })
}

/// 64-bit FNV-1a: names the shared copy. A collision costs a rewrite, never a
/// wrong stand-in: [`is_stand_in`] compares the bytes.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

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
    fn keeping_a_directory_transfers_cleanup_to_the_caller() {
        let parent = tempdir().expect("parent");
        let dir = TempDir::new_in(parent.path()).expect("child");
        fs::write(dir.path().join("artifact"), b"retained").expect("artifact");
        let kept = dir.keep();
        assert_eq!(
            fs::read(kept.join("artifact")).expect("retained"),
            b"retained"
        );
        fs::remove_dir_all(kept).expect("caller cleanup");
        assert_eq!(fs::read_dir(parent.path()).expect("parent").count(), 0);
    }

    #[test]
    fn builder_prefix() {
        let dir = Builder::new()
            .prefix("myprefix_")
            .tempdir()
            .expect("create");
        let name = dir.path().file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("myprefix_"), "name: {name}");
        let inner = Builder::new()
            .prefix("inner_")
            .tempdir_in(dir.path())
            .expect("create in dir");
        assert_eq!(inner.path().parent(), Some(dir.path()));
        let name = inner.path().file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("inner_"), "name: {name}");
    }

    #[test]
    fn a_prefix_cannot_escape_the_requested_parent() {
        let root = tempdir().expect("root");
        let parent = root.path().join("inside");
        fs::create_dir(&parent).expect("parent");
        let absolute = root.path().join("outside_");
        for prefix in ["../outside_", "nested/child_", absolute.to_str().unwrap()] {
            let error = Builder::new()
                .prefix(prefix)
                .tempdir_in(&parent)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{prefix:?}");
        }
        #[cfg(windows)]
        for prefix in [r"..\outside_", r"C:outside_"] {
            let error = Builder::new()
                .prefix(prefix)
                .tempdir_in(&parent)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{prefix:?}");
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
        for prefix in ["", ".", "..", "ordinary_"] {
            let dir = Builder::new().prefix(prefix).tempdir_in(&parent).unwrap();
            assert_eq!(dir.path().parent(), Some(parent.as_path()));
        }
    }

    /// One body, one read-only inode (on macOS); another body, its own; a shared
    /// copy whose mode or bytes changed is replaced, not linked; a body laid
    /// again where it lies leaves nothing beside it.
    #[cfg(unix)]
    #[test]
    fn stand_ins_share_one_read_only_inode_per_body() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        #[cfg(target_os = "macos")]
        struct Evict(PathBuf);
        #[cfg(target_os = "macos")]
        impl Drop for Evict {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let dir = tempdir().expect("tempdir");
        // This run's own body, so no concurrent run tampers with its shared
        // copy; the copy goes with the test, pass or fail.
        let body = format!("#!/bin/sh\necho {}\n", unique_name("t"));
        #[cfg(target_os = "macos")]
        let _evict = Evict(
            call0(std::env::temp_dir)
                .join("aterm-stand-ins")
                .join(format!("{:016x}", fnv1a(body.as_bytes()))),
        );
        let [a, b, c] = ["a", "b", "c"].map(|name| dir.path().join(name));
        stand_in(&a, &body).expect("a");
        stand_in(&b, &body).expect("b");
        stand_in(&c, "#!/bin/sh\necho other\n").expect("c");
        let meta = |p: &Path| fs::metadata(p).expect("stand-in");
        for p in [&a, &b] {
            assert_eq!(fs::read_to_string(p).expect("read"), body);
            assert_eq!(meta(p).mode() & 0o7777, 0o555);
        }
        assert!(
            fs::write(&a, "x").is_err(),
            "a write through a stand-in fails"
        );
        assert_ne!(meta(&a).ino(), meta(&c).ino());
        if cfg!(target_os = "macos") {
            assert_eq!(meta(&a).ino(), meta(&b).ino(), "one inode per body");
            fs::set_permissions(&a, fs::Permissions::from_mode(0o755)).expect("chmod");
            stand_in(&b, &body).expect("b over a shared copy made writable");
            assert_ne!(meta(&a).ino(), meta(&b).ino());
            assert_eq!(meta(&b).mode() & 0o7777, 0o555);
            fs::set_permissions(&b, fs::Permissions::from_mode(0o755)).expect("chmod");
            fs::write(&b, body.replace("echo", "eCho")).expect("same length, other bytes");
            fs::set_permissions(&b, fs::Permissions::from_mode(0o555)).expect("chmod back");
            stand_in(&a, &body).expect("a over a shared copy with other bytes");
            assert_eq!(fs::read_to_string(&a).expect("read"), body);
        }
        #[cfg(target_os = "macos")]
        {
            let ro = tempdir().expect("tempdir");
            fs::set_permissions(ro.path(), fs::Permissions::from_mode(0o555)).expect("chmod");
            let shared = meta(&_evict.0).ino();
            assert!(stand_in(&ro.path().join("x"), &body).is_err());
            assert_eq!(
                meta(&_evict.0).ino(),
                shared,
                "a failed link keeps the copy"
            );
            fs::set_permissions(ro.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
        }
        stand_in(&c, &body).expect("c replaced by rename");
        assert_eq!(fs::read_to_string(&c).expect("read"), body);
        stand_in(&c, &body).expect("c laid again, unchanged");
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .expect("list")
            .map(|e| e.expect("entry").file_name().into_string().expect("UTF-8"))
            .collect();
        names.sort();
        assert_eq!(names, ["a", "b", "c"]);
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
