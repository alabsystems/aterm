// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zero-dependency temporary directories with RAII cleanup.

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

#[cfg(unix)]
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a temporary name component from the pid, a process-wide counter,
/// and OS randomness (injective within one process; [`TempDir::made_in`]
/// retries on cross-process collision).
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
#[cfg(unix)]
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
        Self::made_in_with(dir, || unique_name(prefix))
    }

    fn made_in_with(dir: &Path, mut next_name: impl FnMut() -> String) -> io::Result<Self> {
        for _ in 0..5 {
            let name = next_name();
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

/// The name, before the user id, of the folder in the temporary directory's quiet folder that
/// holds a user's shared stand-ins ([`shared_stand_in`]). aterm-verify's stand-ins made it
/// first; the name stays, so every stand-in shares the files they made.
#[cfg(unix)]
const STAND_IN_CACHE: &str = "atv-standins-";

/// THE ONE NAME OF A STAND-IN'S FILE: the file holding `text` in this user's cache in the
/// temporary directory's quiet folder ([`quiet_in`]) that the stand-ins of `text` this process
/// lays are symlinks to (`stand_in` on macOS, aterm-verify's `standin` on every unix). It is the
/// first of the cache's slots that holds `text` whole, made when none does yet (`cached_in`), so
/// a later process finds that same file; first writers that raced may each run a slot of their
/// own (up to four), and a stand-in written in place runs none. It stays this process's file for
/// `text` while it still holds `text`: each call checks it again (`holds`), and one that no
/// longer does (made writable and written through, or removed) is passed over for the slot
/// `cached_in` gives next. `None` when the cache cannot be used; the stand-in is then written
/// in place.
///
/// macOS assesses the first exec of each file, so a test pays one assessment per text, not one
/// per stand-in. That file's one name is never removed. macOS reads an exec's file by name to
/// assess it, and a file with several names (hard links, one per stand-in) need not be named by
/// the link an exec ran it through: on 27.0.1, once one fixture had opened its link, an exec
/// through another fixture's left the kernel naming the file, by its id, by the first fixture's
/// link. A fixture removing its own (its directory going at its end) could so leave the
/// assessment of another fixture's exec reading a name that was gone; syspolicyd then ended that
/// stand-in as malware (`killed by signal 9`, an exec security-policy exit). A symlink is no
/// name of the file: an exec through it runs the file by its one name, and `$0` is still the
/// stand-in's own path, so a body finds what lies beside it.
///
/// Test support that aterm-verify's `standin` reaches in every profile it builds in, so it is
/// not gated by profile; no shipped code calls it.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
#[doc(hidden)]
#[must_use]
pub fn shared_stand_in(text: &str) -> Option<PathBuf> {
    use std::sync::{Mutex, PoisonError};
    static SHARED: Mutex<BTreeMap<String, PathBuf>> = Mutex::new(BTreeMap::new());
    let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    shared_in(&mut shared, euid(), &call0(std::env::temp_dir), text)
}

/// [`shared_stand_in`] over the memo `shared`, for user `me` and the temporary directory `tmp`:
/// the file `shared` names for `text` while it still [`holds`] `text`, else `cached_in`'s, which
/// `shared` names from then on; `None`, and `text` unnamed, when the cache cannot be used.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn shared_in(
    shared: &mut BTreeMap<String, PathBuf>,
    me: u32,
    tmp: &Path,
    text: &str,
) -> Option<PathBuf> {
    if let Some(file) = shared.get(text) {
        if holds(me, file, text) {
            return Some(file.clone());
        }
        shared.remove(text);
    }
    let file = cached_in(me, tmp, text)?;
    shared.insert(text.to_owned(), file.clone());
    Some(file)
}

/// Whether `file` is a slot a stand-in of `text` may run: a regular file of `me`'s (not a link
/// to one), mode 0555, holding exactly `text`.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn holds(me: u32, file: &Path, text: &str) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    fs::symlink_metadata(file)
        .is_ok_and(|m| m.is_file() && m.uid() == me && m.mode() & 0o7777 == 0o555)
        && fs::read(file).is_ok_and(|read| read == text.as_bytes())
}

/// The file holding `text` in user `me`'s stand-in cache in `tmp`'s quiet folder ([`quiet_in`]),
/// made when none of its slots holds it yet; `None` when the cache cannot be used.
/// [`shared_stand_in`]'s `me` is this process's effective uid. The cache folder is used only
/// when it is a directory, not a link to one, of `me`'s with no permission bits for group or
/// others, so another user's folder of its name (in a shared `/tmp`, made before this user's) is
/// refused.
///
/// The cache holds one file per text (a slot more only where a writer died mid-write or two
/// first writers raced), so it stays as small as the set of texts, and no file in it is ever
/// renamed, linked or removed. A file is made under its own name with no exec bit, written,
/// closed, and only then made mode 0555. A slot is used only when it is a regular file of `me`'s,
/// mode 0555, holding exactly `text`, so no process runs one still being written (an exec of a
/// file open for writing is `ETXTBSY` on Linux), and a slot that is not becomes the next slot's
/// turn.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn cached_in(me: u32, tmp: &Path, text: &str) -> Option<PathBuf> {
    use std::io::Write as _;
    use std::os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    };
    let cache = quiet_in(tmp).join(format!("{STAND_IN_CACHE}{me}"));
    if let Err(e) = fs::DirBuilder::new().mode(0o700).create(&cache)
        && e.kind() != io::ErrorKind::AlreadyExists
    {
        return None;
    }
    let Ok(dir) = fs::symlink_metadata(&cache) else {
        return None;
    };
    if !dir.is_dir() || dir.uid() != me || dir.mode() & 0o077 != 0 {
        return None;
    }
    let hash = fnv1a(text.as_bytes());
    for slot in 0..4 {
        let file = cache.join(format!("{hash:016x}-{slot}"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
        {
            Ok(mut made) => {
                let written = made.write_all(text.as_bytes());
                drop(made);
                return (written.is_ok()
                    && fs::set_permissions(&file, fs::Permissions::from_mode(0o555)).is_ok())
                .then_some(file);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if holds(me, &file, text) {
                    return Some(file);
                }
            }
            Err(_) => return None,
        }
    }
    None
}

/// Lay an executable stand-in at `path` holding `body`, mode 0555, replacing whatever is there by rename.
///
/// On macOS `path` is a symlink to the one file of `body` this user's stand-ins share
/// ([`shared_stand_in`]), made at a name of its own beside `path` and renamed over it. Off
/// macOS, and where that cache cannot be used, `path` is a file of its own holding `body`.
/// Either way `path` reads and runs as `body`, and `$0` in it is `path`. A caller that needs a
/// regular file of its own at `path` (one a program judges by its type or by where it resolves)
/// writes that file itself.
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
    let shared =
        shared_stand_in(body).is_some_and(|file| std::os::unix::fs::symlink(file, &tmp).is_ok());
    #[cfg(not(target_os = "macos"))]
    let shared = false;
    if !shared && let Err(e) = write_read_only(&tmp, body.as_bytes()) {
        return Err(e);
    }
    let renamed = fs::rename(&tmp, path);
    if renamed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    renamed
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

/// 64-bit FNV-1a, aterm-hash's `fnv1a_64` (this crate depends on nothing): names a text's slots
/// in the stand-in cache ([`cached_in`]), as aterm-verify's stand-ins named them. A collision
/// costs a slot, never a wrong stand-in: a slot is used only when it holds the text's bytes.
#[cfg(unix)]
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

    /// The bodies the stand-in tests lay: fixed texts, so the cache every process of this user
    /// shares gains one file for each once, never one a run.
    #[cfg(unix)]
    const RUNS: &str = "#!/bin/sh\necho \"$0 $*\"\n";
    #[cfg(unix)]
    const OTHER: &str = "#!/bin/sh\necho other\n";

    /// ONE NAME PER STAND-IN'S FILE. Two stand-ins of one body, in two fixtures, run one file (on
    /// macOS, where they share it) outside both fixtures, read-only and executable, whose link
    /// count is 1: no fixture holds a name of it, so the first fixture going, its directory and
    /// all, leaves the second's stand-in running that body as itself (`$0` is its own path).
    /// Another body runs a file of its own. NEGATIVE CONTROL: laid by hard link, as the copy this
    /// replaced laid them, that file has a name in each fixture besides its own, so its link
    /// count is 3 (more while another test links it); and the gone fixture's stand-in does not
    /// run.
    #[cfg(unix)]
    #[test]
    fn stand_ins_of_one_body_run_one_file_of_one_name() {
        use std::os::unix::fs::MetadataExt as _;
        let (one, two) = (tempdir().expect("a fixture"), tempdir().expect("another"));
        let a = one.path().join("tool");
        let b = two.path().join("tool");
        let c = two.path().join("other");
        stand_in(&a, RUNS).expect("a");
        stand_in(&b, RUNS).expect("b");
        stand_in(&c, OTHER).expect("c");
        let runs = |p: &Path| fs::canonicalize(p).expect("a stand-in resolves");
        for (p, body) in [(&a, RUNS), (&b, RUNS), (&c, OTHER)] {
            let meta = fs::metadata(p).expect("a stand-in's file");
            assert_eq!(fs::read_to_string(p).expect("read"), body);
            assert_eq!(meta.mode() & 0o7777, 0o555, "{}", p.display());
            assert_eq!(
                meta.nlink(),
                1,
                "{} has a name a fixture removes",
                runs(p).display()
            );
        }
        assert!(
            fs::write(&a, "x").is_err(),
            "a write through a stand-in fails"
        );
        assert_ne!(runs(&a), runs(&c), "another body, another file");
        if cfg!(target_os = "macos") {
            assert_eq!(runs(&a), runs(&b), "one file per body");
            for fixture in [one.path(), two.path()] {
                let fixture = fs::canonicalize(fixture).expect("a fixture resolves");
                assert!(
                    !runs(&a).starts_with(&fixture),
                    "{} runs from inside {}",
                    a.display(),
                    fixture.display()
                );
            }
        }
        drop(one);
        let run = |tool: &Path| {
            aterm_uds::deadline::output_within(
                aterm_uds::spawnfd::command(tool).arg("kept"),
                aterm_uds::deadline::FIRST_EXEC_HANG,
            )
        };
        let out = run(&b).expect("the kept fixture's stand-in ran");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{} kept\n", b.display())
        );
        assert!(run(&a).is_err(), "the gone fixture's stand-in ran");
    }

    /// A stand-in laid again with another body runs that body, by rename; laid again with its
    /// own body it is unchanged, and nothing is left beside it. The file of the body it ran
    /// before still holds that body (on macOS, where it is shared): no stand-in's file is
    /// rewritten. A folder it cannot write gets no stand-in (`PermissionDenied`, the temp's own
    /// error), nothing is left in it, and the file of that body is unchanged. NEGATIVE CONTROL:
    /// a path naming no file is refused, and nothing is laid.
    #[cfg(unix)]
    #[test]
    fn a_stand_in_laid_again_with_another_body_runs_that_body() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = tempdir().expect("a fixture");
        let tool = dir.path().join("tool");
        stand_in(&tool, RUNS).expect("the first body");
        let ran = fs::canonicalize(&tool).expect("the stand-in resolves");
        stand_in(&tool, OTHER).expect("another body");
        assert_eq!(fs::read_to_string(&tool).expect("read"), OTHER);
        if cfg!(target_os = "macos") {
            assert_eq!(fs::read_to_string(&ran).expect("the file it ran"), RUNS);
        }
        stand_in(&tool, OTHER).expect("laid again, unchanged");
        assert_eq!(fs::read_to_string(&tool).expect("read"), OTHER);
        let refused = stand_in(&dir.path().join(".."), RUNS).map_err(|e| e.kind());
        assert_eq!(refused, Err(io::ErrorKind::InvalidInput));
        let names: Vec<String> = fs::read_dir(dir.path())
            .expect("list")
            .map(|e| e.expect("entry").file_name().into_string().expect("UTF-8"))
            .collect();
        assert_eq!(names, ["tool"]);
        if euid() != 0 {
            let file = fs::canonicalize(&tool).expect("the stand-in resolves");
            let before = fs::metadata(&file).map(|m| (m.ino(), m.mode(), m.mtime_nsec()));
            let shut = tempdir().expect("a fixture it cannot write");
            fs::set_permissions(shut.path(), fs::Permissions::from_mode(0o555)).expect("shut");
            let refused = stand_in(&shut.path().join("tool"), OTHER).map_err(|e| e.kind());
            let left = fs::read_dir(shut.path()).map(Iterator::count);
            fs::set_permissions(shut.path(), fs::Permissions::from_mode(0o700)).expect("open");
            assert_eq!(refused, Err(io::ErrorKind::PermissionDenied));
            assert_eq!(
                left.ok(),
                Some(0),
                "nothing is left in a folder it cannot write"
            );
            assert_eq!(fs::read_to_string(&file).expect("read"), OTHER);
            let after = fs::metadata(&file).map(|m| (m.ino(), m.mode(), m.mtime_nsec()));
            assert_eq!(after.ok(), before.ok(), "{} changed", file.display());
        }
    }

    /// A FILE THAT NO LONGER HOLDS ITS TEXT IS RUN NO MORE. The memo's file for a text, made
    /// writable and written through, is passed over: the next ask gets the next slot, holding the
    /// text, and the written-through file is left as it is; that file removed in turn, the next
    /// ask gets one holding the text again, which the memo then names. NEGATIVE CONTROL: the
    /// memo's file, untouched, is the one given again.
    #[cfg(unix)]
    #[test]
    fn a_stand_in_file_that_no_longer_holds_its_text_is_passed_over() {
        use std::os::unix::fs::PermissionsExt as _;
        let me = euid();
        let tmp = tempdir().expect("scratch");
        let text = "#!/bin/sh\nexit 7\n";
        let tampered = "#!/bin/sh\necho tampered\n";
        let mut shared = BTreeMap::new();
        let first = shared_in(&mut shared, me, tmp.path(), text).expect("a file");
        assert_eq!(
            shared_in(&mut shared, me, tmp.path(), text).as_ref(),
            Some(&first),
            "the same file while it holds the text"
        );
        fs::set_permissions(&first, fs::Permissions::from_mode(0o755)).expect("made writable");
        fs::write(&first, tampered).expect("written through");
        let next = shared_in(&mut shared, me, tmp.path(), text).expect("another file");
        assert_ne!(
            next,
            first,
            "{} is run though it was written",
            first.display()
        );
        assert_eq!(fs::read_to_string(&next).ok().as_deref(), Some(text));
        assert_eq!(fs::read_to_string(&first).ok().as_deref(), Some(tampered));
        fs::remove_file(&next).expect("removed");
        let again = shared_in(&mut shared, me, tmp.path(), text).expect("a file again");
        assert_eq!(fs::read_to_string(&again).ok().as_deref(), Some(text));
        assert_eq!(
            shared.get(text),
            Some(&again),
            "the memo names the file it gave"
        );
    }

    /// Set, the test binary is [`stand_in_cache_child_if_asked`]: it asks [`cached_in`] for this
    /// directory's file holding [`CACHED`] and writes the file it got to `got` there.
    #[cfg(unix)]
    const CACHE_CHILD: &str = "ATERM_TEMPFILE_STAND_IN_CACHE_CHILD";
    /// The text [`the_stand_in_cache_holds_one_read_only_file_per_text`] and its child ask for.
    #[cfg(unix)]
    const CACHED: &str = "#!/bin/sh\nexit 3\n";
    /// The wait for that child, which must end: a one-minute hang detector (aterm-spec's
    /// `wait::MUST_HAPPEN`), never a latency budget.
    #[cfg(unix)]
    const MUST_HAPPEN: std::time::Duration = std::time::Duration::from_secs(60);

    /// Run by [`the_stand_in_cache_holds_one_read_only_file_per_text`] in a process of its own.
    #[cfg(unix)]
    fn stand_in_cache_child_if_asked() -> bool {
        use std::os::unix::ffi::OsStrExt as _;
        let Some(dir) = std::env::var_os(CACHE_CHILD).map(PathBuf::from) else {
            return false;
        };
        let file = cached_in(euid(), &dir, CACHED).expect("cached");
        fs::write(dir.join("got"), file.as_os_str().as_bytes()).expect("said which file");
        true
    }

    /// The stand-in cache keeps one file per text, however often it is asked, by this process
    /// or another, read-only, in a quiet folder on macOS, and nothing else. NEGATIVE CONTROL: a
    /// second text gets a file of its own.
    #[cfg(unix)]
    #[test]
    fn the_stand_in_cache_holds_one_read_only_file_per_text() {
        if stand_in_cache_child_if_asked() {
            return;
        }
        use std::os::unix::ffi::OsStringExt as _;
        use std::os::unix::fs::MetadataExt as _;
        let me = euid();
        let tmp = tempdir().expect("scratch");
        let a = cached_in(me, tmp.path(), CACHED).expect("a cached stand-in");
        let again = cached_in(me, tmp.path(), CACHED).expect("the same text");
        let b = cached_in(me, tmp.path(), "#!/bin/sh\nexit 4\n").expect("another text");
        let child = aterm_uds::deadline::output_within(
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args([
                    "--exact",
                    "tests::the_stand_in_cache_holds_one_read_only_file_per_text",
                    "--test-threads=1",
                ])
                .env(CACHE_CHILD, tmp.path()),
            MUST_HAPPEN,
        )
        .expect("the child ran");
        assert!(child.status.success(), "the child failed: {child:?}");
        let theirs = fs::read(tmp.path().join("got"))
            .map(|got| PathBuf::from(std::ffi::OsString::from_vec(got)));
        assert_eq!(
            theirs.ok().as_ref(),
            Some(&a),
            "another process gets the same file"
        );
        assert_eq!(a, again, "one file per text");
        assert_ne!(a, b, "a text of its own gets a file of its own");
        let cache = a.parent().expect("the cache").to_path_buf();
        let held = fs::read_dir(&cache).expect("list the cache").count();
        assert_eq!(
            held,
            2,
            "{} holds a file per text and nothing else",
            cache.display()
        );
        let mode = fs::metadata(&a).map(|m| m.mode() & 0o7777);
        assert_eq!(mode.ok(), Some(0o555), "read-only and executable");
        if cfg!(target_os = "macos") {
            assert!(is_quiet(&cache), "{} is not quiet", cache.display());
        }
    }

    /// A slot not holding the text, or holding it still writable (its writer has not finished
    /// it), is passed over and left as it is: the text goes in the next slot. NEGATIVE CONTROL:
    /// the next call finds that slot again, and the passed-over ones still hold what they held.
    #[cfg(unix)]
    #[test]
    fn a_slot_still_being_written_or_holding_another_text_is_passed_over() {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        let me = euid();
        let tmp = tempdir().expect("scratch");
        let text = "#!/bin/sh\nexit 5\n";
        let cache = quiet_in(tmp.path()).join(format!("{STAND_IN_CACHE}{me}"));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&cache)
            .expect("the cache");
        let slot = |n: u32| cache.join(format!("{:016x}-{n}", fnv1a(text.as_bytes())));
        let lay = |n: u32, bytes: &str, mode: u32| {
            fs::write(slot(n), bytes).expect("lay a slot");
            fs::set_permissions(slot(n), fs::Permissions::from_mode(mode)).expect("chmod a slot");
        };
        lay(0, "#!/bin/sh\nexit", 0o555);
        lay(1, text, 0o600);
        let got = cached_in(me, tmp.path(), text);
        assert_eq!(got, Some(slot(2)), "the first slot holding the text whole");
        assert_eq!(
            cached_in(me, tmp.path(), text),
            Some(slot(2)),
            "found again"
        );
        assert_eq!(
            fs::read_to_string(slot(0)).ok().as_deref(),
            Some("#!/bin/sh\nexit")
        );
        assert_eq!(fs::read_to_string(slot(1)).ok().as_deref(), Some(text));
    }

    /// A cache folder that is not the user's alone is refused and nothing is made in it: one
    /// another user owns (the folder this process made, asked for as a user it is not, as a
    /// shared `/tmp` can hold another user's folder of this user's name), one others may write,
    /// and a symlink to a private folder of the user's own. NEGATIVE CONTROL: the folder others
    /// could write, made private again, is used.
    #[cfg(unix)]
    #[test]
    fn a_cache_folder_not_the_users_alone_is_refused() {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        let me = euid();
        let text = "#!/bin/sh\nexit 6\n";
        let cache = |tmp: &Path, uid: u32| quiet_in(tmp).join(format!("{STAND_IN_CACHE}{uid}"));
        let held = |dir: &Path| fs::read_dir(dir).map(Iterator::count).ok();
        let private = |dir: &Path| {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(dir)
                .expect("a private folder");
        };

        let theirs = tempdir().expect("scratch");
        let other = me.wrapping_add(1);
        assert_eq!(
            cached_in(other, theirs.path(), text),
            None,
            "another user's"
        );
        assert_eq!(
            held(&cache(theirs.path(), other)),
            Some(0),
            "nothing in theirs"
        );

        let open = tempdir().expect("scratch");
        let folder = cache(open.path(), me);
        private(&folder);
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o777)).expect("open it to others");
        assert_eq!(
            cached_in(me, open.path(), text),
            None,
            "one others may write"
        );
        assert_eq!(held(&folder), Some(0), "nothing in one others may write");
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).expect("make it private");
        let used = cached_in(me, open.path(), text);
        assert_eq!(
            used.as_deref().and_then(Path::parent),
            Some(folder.as_path())
        );

        let linked = tempdir().expect("scratch");
        let elsewhere = linked.path().join("elsewhere");
        private(&elsewhere);
        std::os::unix::fs::symlink(&elsewhere, cache(linked.path(), me)).expect("the link");
        assert_eq!(cached_in(me, linked.path(), text), None, "a symlink");
        assert_eq!(held(&elsewhere), Some(0), "nothing through the symlink");
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
    /// folders. `/tmp`'s may be one another program made (a `mkdir -p` of scratch under it,
    /// `0755`), which serves while it is safe, so its mode is not held here: the `1777` one
    /// made in a sticky root gets is `a_quiet_folder_is_used_only_while_it_is_safe`'s. NEGATIVE
    /// CONTROL: a parent the caller chose gets its directory directly, and no `.ubd` beside it.
    #[cfg(target_os = "macos")]
    #[test]
    fn tempdirs_in_shared_roots_land_where_fseventsd_drops_their_events() {
        let dir = tempdir().expect("tempdir");
        assert!(is_quiet(dir.path()), "{:?}", dir.path());
        assert!(dir.path().starts_with(call0(std::env::temp_dir)));
        let short = Builder::new()
            .prefix("atq-")
            .tempdir_in("/tmp")
            .expect("in /tmp");
        assert_eq!(short.path().parent(), Some(Path::new("/tmp/.ubd")));
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
    /// One this user's `mkdir -p` left (`0755`) is closed, so it serves. A sticky root reached
    /// through a link, as `/tmp` is, gets a folder every user may share. NEGATIVE CONTROL: a
    /// link in its place, or one anyone may write and not sticky, is refused, so the root
    /// itself serves.
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
        chmod(0o755).expect("as mkdir -p leaves it");
        assert!(made_quiet(root.path(), &quiet), "found closed");
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
        let linked_root = root.path().join("linked");
        fs::create_dir(&linked_root).expect("a second sticky root");
        fs::set_permissions(&linked_root, fs::Permissions::from_mode(0o1777)).expect("sticky");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&linked_root, &link).expect("a link to it, as /tmp is");
        assert!(made_quiet(&link, &link.join(QUIET)));
        let mode = fs::metadata(link.join(QUIET)).expect("stat").mode();
        assert_eq!(mode & 0o7777, 0o1777, "through a link: {mode:o}");
    }

    #[test]
    fn made_in_sets_private_mode_retries_collisions_and_propagates_errors() {
        let root = tempdir().expect("root");
        let fresh = TempDir::new_in(root.path()).expect("fresh");
        assert!(fresh.path().is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let mode = fs::metadata(fresh.path()).expect("stat").mode() & 0o7777;
            assert_eq!(mode, 0o700, "Unix tempdirs are mode 0700: {mode:o}");
        }

        let colliding = root.path().join("taken");
        fs::create_dir(&colliding).expect("taken");
        let mut tries = 0usize;
        let won = TempDir::made_in_with(root.path(), || {
            tries += 1;
            if tries < 5 {
                "taken".to_string()
            } else {
                "free_on_fifth".to_string()
            }
        })
        .expect("fifth attempt succeeds");
        assert_eq!(tries, 5);
        assert_eq!(won.path(), root.path().join("free_on_fifth"));

        let mut exhausted_tries = 0usize;
        let exhausted = TempDir::made_in_with(root.path(), || {
            exhausted_tries += 1;
            "taken".to_string()
        })
        .unwrap_err();
        assert_eq!(exhausted_tries, 5);
        assert_eq!(exhausted.kind(), io::ErrorKind::AlreadyExists);

        let mut err_tries = 0usize;
        let missing = TempDir::made_in_with(&root.path().join("missing_parent"), || {
            err_tries += 1;
            "candidate".to_string()
        })
        .unwrap_err();
        assert_eq!(err_tries, 1, "non-collision errors return immediately");
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }

    /// On Windows the OS RNG (BCryptGenRandom) succeeds on empty and 8-byte buffers.
    #[cfg(windows)]
    #[test]
    fn windows_os_random_succeeds() {
        let mut empty = [];
        let mut buf = [0u8; 8];
        assert!(
            read_os_random(&mut empty),
            "empty BCryptGenRandom read failed"
        );
        assert!(
            read_os_random(&mut buf),
            "8-byte BCryptGenRandom read failed"
        );
    }
}
