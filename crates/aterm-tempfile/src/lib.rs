// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zero-dependency temporary directories with RAII cleanup.

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a temporary name component from the pid, a process-wide counter
/// and OS randomness.
#[cfg_attr(trust_verify, trust::skip)]
fn unique_name(prefix: &str) -> String {
    let pid = std::process::id();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let rand = os_random_u64();
    let mut name = String::with_capacity(64);
    name.push_str(prefix);
    name.push_str(&pid.to_string());
    name.push('_');
    let mut v = rand;
    let mut hex = ['0'; 16];
    for slot in hex.iter_mut().rev() {
        *slot = hex_digit(v & 0xf);
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
#[inline]
#[must_use]
pub const fn hex_digit(nibble: u64) -> char {
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

/// Encode a temporary directory seed (`rand_u64`, `pid`, `counter`) into 16 LE bytes.
#[inline]
#[must_use]
pub const fn encode_temp_seed_le(rand_u64: u64, pid: u32, counter: u32) -> [u8; 16] {
    [
        rand_u64 as u8,
        (rand_u64 >> 8) as u8,
        (rand_u64 >> 16) as u8,
        (rand_u64 >> 24) as u8,
        (rand_u64 >> 32) as u8,
        (rand_u64 >> 40) as u8,
        (rand_u64 >> 48) as u8,
        (rand_u64 >> 56) as u8,
        pid as u8,
        (pid >> 8) as u8,
        (pid >> 16) as u8,
        (pid >> 24) as u8,
        counter as u8,
        (counter >> 8) as u8,
        (counter >> 16) as u8,
        (counter >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into a temporary directory seed (`rand_u64`, `pid`, `counter`).
#[inline]
#[must_use]
pub const fn decode_temp_seed_le(bytes: [u8; 16]) -> (u64, u32, u32) {
    let rand_u64 = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let pid = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let counter = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (rand_u64, pid, counter)
}

/// Read 8 bytes of randomness from the OS.
fn os_random_u64() -> u64 {
    let mut buf = [0u8; 8];
    if read_os_random(&mut buf) {
        (buf[0] as u64)
            | ((buf[1] as u64) << 8)
            | ((buf[2] as u64) << 16)
            | ((buf[3] as u64) << 24)
            | ((buf[4] as u64) << 32)
            | ((buf[5] as u64) << 40)
            | ((buf[6] as u64) << 48)
            | ((buf[7] as u64) << 56)
    } else {
        no_csprng_fallback()
    }
}

/// The name seed to use when the OS CSPRNG is unavailable: a nanosecond wall
/// clock.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn no_csprng_fallback() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d
            .as_secs()
            .wrapping_mul(1_000_000_000)
            .wrapping_add(d.subsec_nanos() as u64),
        Err(_) => 0,
    }
}

/// `wasm32-unknown-unknown` arm of [`no_csprng_fallback`], the one target
/// where the clock arm above cannot be used.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn no_csprng_fallback() -> u64 {
    static SEED: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let mut z = SEED.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    z = (z ^ (z / 0x4000_0000)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z / 0x0800_0000)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z / 0x8000_0000)
}

#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn read_os_random(buf: &mut [u8]) -> bool {
    unsafe extern "C" {
        fn getentropy(buf: *mut core::ffi::c_void, len: usize) -> i32;
    }
    // SAFETY: `buf` is a live &mut [u8] for the duration of the call and `len` is its exact length; getentropy writes at most `len` bytes.
    if unsafe { getentropy(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
        return true;
    }
    use std::io::Read;
    match fs::File::open("/dev/urandom") {
        Ok(mut f) => call2(<fs::File as Read>::read_exact, &mut f, buf).is_ok(),
        Err(_) => false,
    }
}

#[cfg(windows)]
#[cfg_attr(trust_verify, trust::skip)]
fn read_os_random(buf: &mut [u8]) -> bool {
    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptGenRandom(h: *mut core::ffi::c_void, p: *mut u8, n: u32, f: u32) -> i32;
    }
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    // SAFETY: buf is a live &mut [u8] for the duration of the call; NULL algorithm handle + the flag is the documented system-RNG form; 0 == STATUS_SUCCESS.
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

/// THE FOLDER FSEVENTSD DROPS. On macOS 27.0 (26A428) fseventsd hands no client, and keeps in
/// no history, a file event whose path holds `/.ubd/` within its first 1024 bytes: its
/// dispatcher returns before it takes its client list when `strnstr(path, "/.ubd/", 1024)`
/// finds one, a leftover of the retired Ubiquity daemon. That is observed, not documented, so
/// a later macOS that stops honouring the name loses only the benefit, never a file
/// (`docs/measured/fseventsd-ubd-2026-10-26.md`). Test scratch was 89% of the file events
/// fseventsd delivered on a build Mac; under this folder the same test run cost fseventsd a
/// ninth of the CPU and left nothing in its history. It is no rule of Spotlight's, which still
/// indexes a dot folder's folders: a `*.noindex` name keeps Spotlight out.
pub const QUIET: &str = ".ubd";

/// Whether fseventsd drops what is made under `dir`: one of its components is [`QUIET`] (the
/// whole component; `x.ubd` and `.ubdx` are not).
#[must_use]
pub fn is_quiet(dir: &Path) -> bool {
    dir.components().any(|c| c.as_os_str() == QUIET)
}

/// `dir`'s [`QUIET`] folder, made when absent, for files kept in a temporary root fseventsd
/// need not hear of. `dir` itself where it is quiet already, off macOS (nothing else drops
/// events by path, so the folder would only lengthen paths), or where the folder cannot be
/// used ([`made_quiet`]).
#[must_use]
pub fn quiet_in(dir: &Path) -> PathBuf {
    match quiet_place(dir, cfg!(target_os = "macos")) {
        Some(quiet) if made_quiet(dir, &quiet) => quiet,
        _ => dir.to_path_buf(),
    }
}

/// The folder a directory asked for in `dir` goes in when `wanted`: `dir`'s [`QUIET`] folder,
/// unless `dir` is quiet already; `None` keeps `dir`.
fn quiet_place(dir: &Path, wanted: bool) -> Option<PathBuf> {
    (wanted && !is_quiet(dir)).then(|| dir.join(QUIET))
}

/// `dir` cut at its first [`QUIET`] folder: `T/.ubd/x` is `T`; a `dir` with none is itself.
/// What is made there reaches fseventsd's clients and its history again.
#[must_use]
pub fn outside_quiet(dir: &Path) -> PathBuf {
    dir.components()
        .take_while(|c| c.as_os_str() != QUIET)
        .collect()
}

/// Where [`TempDir`] makes a directory asked for in `dir`: on macOS, in `dir`'s [`QUIET`]
/// folder ([`quiet_in`]) when `dir` is a shared temporary root ([`is_shared_root`]); in `dir`
/// itself otherwise, so a folder the caller chose holds exactly what it was given.
fn placed(dir: &Path) -> PathBuf {
    if cfg!(target_os = "macos") && is_shared_root(dir) {
        quiet_in(dir)
    } else {
        dir.to_path_buf()
    }
}

/// Whether `dir` is a temporary root other programs write too: the system temporary
/// directory, as named or as it resolves, or `/tmp`.
fn is_shared_root(dir: &Path) -> bool {
    let temp = call0(std::env::temp_dir);
    dir == temp
        || dir == Path::new("/tmp")
        || dir == Path::new("/private/tmp")
        || (dir.starts_with("/private") && fs::canonicalize(&temp).is_ok_and(|t| t == dir))
}

/// Whether the [`QUIET`] folder `quiet` of `dir` stands for this user to make directories in,
/// made when absent: `0700`, or `1777` in a sticky directory every user shares (`/tmp`). One
/// found is used only while it is a real directory (no link) that this user owns and no one
/// else may write unless it is sticky, or that root owns, sticky and open to all as `/tmp` is;
/// another user's in a shared `/tmp` is never used, and `dir` itself serves instead.
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
fn made_quiet(dir: &Path, quiet: &Path) -> bool {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    let Ok(parent) = fs::metadata(dir) else {
        return false;
    };
    let shared = parent.mode() & 0o1000 != 0;
    match fs::DirBuilder::new()
        .mode(if shared { 0o1777 } else { 0o700 })
        .create(quiet)
    {
        // The umask took the bits every user needs to share it; without them it is still
        // this user's own, which the checks below accept.
        Ok(()) if shared => {
            let _ = fs::set_permissions(quiet, fs::Permissions::from_mode(0o1777));
        }
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return false,
    }
    let Ok(found) = fs::symlink_metadata(quiet) else {
        return false;
    };
    let mode = found.mode();
    let sticky = mode & 0o1000 != 0;
    let own = found.uid() == euid() && (mode & 0o022 == 0 || sticky);
    let like_tmp = found.uid() == 0 && sticky && mode & 0o002 != 0;
    found.is_dir() && (own || like_tmp)
}

/// No [`QUIET`] folder off macOS ([`quiet_in`] asks for none there).
#[cfg(not(target_os = "macos"))]
const fn made_quiet(_dir: &Path, _quiet: &Path) -> bool {
    false
}

/// This process's effective user id.
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
fn euid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: `geteuid` takes no argument, touches no memory of this process's and cannot fail.
    unsafe { geteuid() }
}

/// A temporary directory, removed with all its contents on drop.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a new temporary directory in the system temp directory (in its [`QUIET`] folder
    /// on macOS, [`quiet_in`]).
    /// # Errors
    /// Returns an error if the directory cannot be created.
    pub fn new() -> io::Result<Self> {
        Self::new_in(call0(std::env::temp_dir))
    }

    /// Create a new temporary directory inside `dir`, or inside its [`QUIET`] folder where `dir`
    /// is a shared temporary root (the system temp directory, `/tmp`) on macOS.
    /// # Errors
    /// Returns an error if the directory cannot be created.
    pub fn new_in(dir: impl AsRef<Path>) -> io::Result<Self> {
        Self::with_prefix_in(".tmp", dir)
    }

    /// Create a new temporary directory with a custom prefix inside `dir`, or inside its
    /// [`QUIET`] folder where `dir` is a shared temporary root ([`placed`]).
    fn with_prefix_in(prefix: &str, dir: impl AsRef<Path>) -> io::Result<Self> {
        Self::made_in(prefix, &placed(dir.as_ref()))
    }

    /// Create a new temporary directory with a custom prefix in exactly `dir`.
    fn made_in(prefix: &str, dir: &Path) -> io::Result<Self> {
        for _ in 0..5 {
            let name = unique_name(prefix);
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
    #[inline]
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the directory and its contents after this guard is consumed.
    #[inline]
    #[must_use]
    pub fn keep(mut self) -> PathBuf {
        std::mem::take(&mut self.path)
    }
}

impl Drop for TempDir {
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

impl AsRef<Path> for TempDir {
    #[inline]
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// Builder for temporary directories with a custom prefix.
#[derive(Debug, Default)]
pub struct Builder {
    prefix: Option<String>,
    heard: bool,
}

impl Builder {
    /// Create a new builder with default options.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prefix: None,
            heard: false,
        }
    }

    /// Make the directory outside every [`QUIET`] folder ([`outside_quiet`]), where fseventsd
    /// hears of what is made in it: for a subject that refuses a path under a dot folder, which
    /// a quiet folder is, or one that needs the root's own short path.
    #[inline]
    #[must_use]
    pub const fn heard(mut self) -> Self {
        self.heard = true;
        self
    }

    /// Set the prefix for the temporary name.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.prefix = Some(prefix.to_owned());
        self
    }

    /// The configured prefix, or the crate default `.tmp`.
    #[inline]
    const fn prefix_str(&self) -> &str {
        match &self.prefix {
            Some(p) => p.as_str(),
            None => ".tmp",
        }
    }

    /// Create a temporary directory using the configured options.
    /// # Errors
    /// Returns an error if the directory cannot be created.
    pub fn tempdir(&self) -> io::Result<TempDir> {
        self.tempdir_in(call0(std::env::temp_dir))
    }

    /// Create a temporary directory inside `dir` using the configured options (e.g. a short `/tmp` for a socket path, or a target-local scratch root); a shared temporary root's goes in its [`QUIET`] folder on macOS, five bytes longer, unless the builder is [`Self::heard`], which makes it in `dir` cut at its first quiet folder.
    /// # Errors
    /// Returns an error if the directory cannot be created.
    pub fn tempdir_in(&self, dir: impl AsRef<Path>) -> io::Result<TempDir> {
        if self.heard {
            TempDir::made_in(self.prefix_str(), &outside_quiet(dir.as_ref()))
        } else {
            TempDir::with_prefix_in(self.prefix_str(), dir)
        }
    }
}

/// Create a temporary directory in the system temp directory (in its [`QUIET`] folder on macOS).
/// # Errors
/// Returns an error if the directory cannot be created.
pub fn tempdir() -> io::Result<TempDir> {
    TempDir::new()
}

/// Lay an executable stand-in at `path` holding `body`, mode 0555, replacing whatever is there by rename.
/// # Errors
/// Returns an error if `path` has no file name or the stand-in cannot be written or moved into place.
#[cfg(all(unix, any(test, debug_assertions, feature = "test-support")))]
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
    if !shared && let Err(e) = write_read_only(&tmp, body.as_bytes()) {
        return Err(e);
    }
    let renamed = fs::rename(&tmp, path);
    let _ = fs::remove_file(&tmp);
    renamed
}

/// Hard-link the shared copy of `body` at `tmp`, laying it first when it is
/// absent or not `body`.
#[cfg(all(
    target_os = "macos",
    any(test, debug_assertions, feature = "test-support")
))]
#[allow(clippy::question_mark)]
fn link_shared(tmp: &Path, dir: &Path, body: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    let cache = call0(std::env::temp_dir).join("aterm-stand-ins");
    if let Err(e) = fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&cache)
    {
        return Err(e);
    }
    let entry = cache.join(format!("{:016x}", fnv1a(body)));
    if !is_stand_in(&entry, &cache, body) {
        let fresh = cache.join(unique_name(".new."));
        if let Err(e) = write_read_only(&fresh, body) {
            return Err(e);
        }
        if let Err(e) = fs::rename(&fresh, &entry) {
            let _ = fs::remove_file(&fresh);
            return Err(e);
        }
    }
    if let Err(e) = fs::hard_link(&entry, tmp) {
        return Err(e);
    }
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
#[cfg(all(
    target_os = "macos",
    any(test, debug_assertions, feature = "test-support")
))]
fn is_stand_in(file: &Path, dir: &Path, body: &[u8]) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let (Ok(meta), Ok(owner)) = (fs::symlink_metadata(file), fs::metadata(dir)) else {
        return false;
    };
    if !meta.is_file() || meta.uid() != owner.uid() || meta.mode() & 0o7777 != 0o555 {
        return false;
    }
    match fs::read(file) {
        Ok(read) => read == body,
        Err(_) => false,
    }
}

/// Create `path` (it must not exist) holding `body`, then make it mode 0555.
#[cfg(all(unix, any(test, debug_assertions, feature = "test-support")))]
#[allow(clippy::question_mark)]
fn write_read_only(path: &Path, body: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => return Err(e),
    };
    let written = match file.write_all(body) {
        Ok(()) => file.set_permissions(fs::Permissions::from_mode(0o555)),
        Err(e) => Err(e),
    };
    drop(file);
    if written.is_err() {
        let _ = fs::remove_file(path);
    }
    written
}

/// 64-bit FNV-1a: names the shared copy. A collision costs a rewrite, never a
/// wrong stand-in: [`is_stand_in`] compares the bytes.
#[cfg(all(
    target_os = "macos",
    any(test, debug_assertions, feature = "test-support")
))]
const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut i = 0;
    while i < bytes.len() {
        hash = (hash ^ (bytes[i] as u64)).wrapping_mul(0x0100_0000_01b3);
        i += 1;
    }
    hash
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

    /// THE QUIET FOLDER'S SHAPE: a directory asked for in a shared root goes in that root's
    /// `.ubd` folder, so its path holds `/.ubd/`, and one asked for in a folder already under
    /// one goes there as asked (no second `.ubd`). NEGATIVE CONTROL: unwanted (off macOS, or a
    /// folder the caller chose) it stays in the folder asked for, and a name that only holds
    /// `.ubd` is no quiet component.
    #[test]
    fn a_shared_roots_directories_go_in_its_quiet_folder() {
        let tmp = Path::new("/tmp");
        let quiet = quiet_place(tmp, true).expect("a shared root is made quiet");
        assert_eq!(quiet, Path::new("/tmp/.ubd"));
        let made = quiet.join("al-sim1_0123456789abcdef_0");
        assert!(
            made.to_str().is_some_and(|p| p.contains("/.ubd/")),
            "{made:?}"
        );
        assert!(is_quiet(&quiet) && is_quiet(&made));
        assert_eq!(quiet_place(&made, true), None, "no second .ubd");
        assert_eq!(quiet_place(tmp, false), None);
        for loud in ["/tmp", "/tmp/x.ubd/y", "/tmp/.ubdx/y", "/tmp/ubd/y"] {
            assert!(!is_quiet(Path::new(loud)), "{loud}");
        }
        assert!(is_shared_root(tmp) && is_shared_root(&call0(std::env::temp_dir)));
        assert!(!is_shared_root(Path::new("/tmp/x")));
    }

    /// What the tempdirs land in: on macOS the system temp directory's and `/tmp`'s quiet
    /// folders, `/tmp`'s sticky and open to every user. NEGATIVE CONTROL: a parent the caller
    /// chose gets its directory directly, and no `.ubd` beside it.
    #[cfg(target_os = "macos")]
    #[test]
    fn tempdirs_in_shared_roots_land_where_fseventsd_drops_their_events() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempdir().expect("tempdir");
        assert!(is_quiet(dir.path()), "{:?}", dir.path());
        assert!(dir.path().starts_with(call0(std::env::temp_dir)));
        let short = Builder::new()
            .prefix("atq-")
            .tempdir_in("/tmp")
            .expect("in /tmp");
        assert_eq!(short.path().parent(), Some(Path::new("/tmp/.ubd")));
        let mode = fs::symlink_metadata("/tmp/.ubd").expect("lstat").mode();
        assert_eq!(mode & 0o1000, 0o1000, "sticky: {mode:o}");
        let chosen = Path::new("/tmp").join(unique_name("atq-chosen-"));
        fs::create_dir(&chosen).expect("a parent of the caller's");
        let inner = TempDir::new_in(&chosen).expect("in the chosen parent");
        assert_eq!(inner.path().parent(), Some(chosen.as_path()));
        drop(inner);
        assert_eq!(fs::read_dir(&chosen).expect("list").count(), 0);
        fs::remove_dir(&chosen).expect("cleanup");
    }

    /// A heard directory is made outside every quiet folder: `outside_quiet` cuts a path at its
    /// first `.ubd` component. NEGATIVE CONTROL: a component that only holds the name
    /// (`x.ubd`) cuts nothing, and a builder that is not heard places as before.
    #[cfg(unix)]
    #[test]
    fn a_heard_directory_is_made_outside_every_quiet_folder() {
        assert_eq!(outside_quiet(Path::new("/v/T/.ubd/x/y")), Path::new("/v/T"));
        assert_eq!(outside_quiet(Path::new("/v/T/.ubd/")), Path::new("/v/T"));
        assert_eq!(
            outside_quiet(Path::new("/v/T/x.ubd/y")),
            Path::new("/v/T/x.ubd/y")
        );
        assert_eq!(outside_quiet(Path::new("/v/T")), Path::new("/v/T"));
        let heard = Builder::new()
            .prefix("ath-")
            .heard()
            .tempdir()
            .expect("heard");
        assert!(!is_quiet(heard.path()), "{:?}", heard.path());
        assert_eq!(
            heard.path().parent(),
            Some(outside_quiet(&call0(std::env::temp_dir)).as_path())
        );
        let in_tmp = Builder::new()
            .prefix("ath-")
            .heard()
            .tempdir_in("/tmp")
            .expect("/tmp");
        assert_eq!(in_tmp.path().parent(), Some(Path::new("/tmp")));
        let placed = Builder::new()
            .prefix("ath-")
            .tempdir_in("/tmp")
            .expect("/tmp");
        assert_eq!(
            is_quiet(placed.path()),
            cfg!(target_os = "macos"),
            "{:?}",
            placed.path()
        );
    }

    /// A quiet folder is used only while it is safe to: this user's own and closed, or sticky.
    /// NEGATIVE CONTROL: a link in its place, or one anyone may write and not sticky, is
    /// refused, so the root itself serves.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_quiet_folder_is_used_only_while_it_is_safe() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let root = tempdir().expect("root");
        let quiet = root.path().join(QUIET);
        assert!(made_quiet(root.path(), &quiet), "made");
        assert_eq!(fs::metadata(&quiet).expect("stat").mode() & 0o7777, 0o700);
        assert!(made_quiet(root.path(), &quiet), "found again");
        let chmod = |mode| fs::set_permissions(&quiet, fs::Permissions::from_mode(mode));
        chmod(0o777).expect("open it");
        assert!(!made_quiet(root.path(), &quiet), "open and not sticky");
        chmod(0o1777).expect("sticky");
        assert!(made_quiet(root.path(), &quiet), "sticky");
        fs::remove_dir(&quiet).expect("rmdir");
        std::os::unix::fs::symlink(root.path(), &quiet).expect("a link in its place");
        assert!(!made_quiet(root.path(), &quiet), "a link");
        fs::remove_file(&quiet).expect("unlink");
        let shared = root.path().join("shared");
        fs::create_dir(&shared).expect("a shared parent");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).expect("sticky parent");
        assert!(made_quiet(&shared, &shared.join(QUIET)));
        let mode = fs::metadata(shared.join(QUIET)).expect("stat").mode();
        assert_eq!(mode & 0o7777, 0o1777, "{mode:o}");
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
