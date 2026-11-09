// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zero-dependency private temporary files and directories.

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

#[cfg(unix)]
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

mod build_paths;
pub use build_paths::{bin_exe, manifest_dir, target_tmpdir};

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
    let Ok(len) = u32::try_from(buf.len()) else {
        return false;
    };
    // SAFETY: buf is a live &mut [u8] for the duration of the call and `len` is its exact length; NULL algorithm handle + the flag is the documented system-RNG form; 0 == STATUS_SUCCESS.
    unsafe {
        BCryptGenRandom(
            core::ptr::null_mut(),
            buf.as_mut_ptr(),
            len,
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
/// What is made there reaches fseventsd's clients and its history again. A relative path that
/// starts with [`QUIET`] cuts to `.`.
#[must_use]
pub fn outside_quiet(dir: &Path) -> PathBuf {
    let cut: PathBuf = dir
        .components()
        .take_while(|c| c.as_os_str() != QUIET)
        .collect();
    if cut.as_os_str().is_empty() && !dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        cut
    }
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
            if name.contains('\0')
                || (cfg!(windows) && (name.ends_with('.') || name.ends_with(' ')))
                || !matches!(components.next(), Some(std::path::Component::Normal(part)) if part == name.as_str())
                || components.next().is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a temporary directory name must not contain a path",
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

/// Builder for temporary directories with a custom prefix and suffix.
#[derive(Debug, Default)]
pub struct Builder {
    prefix: Option<String>,
    suffix: Option<String>,
    heard: bool,
}

impl Builder {
    /// Create a new builder with default options.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prefix: None,
            suffix: None,
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

    /// Set the suffix after the unique temporary name, such as `.noindex`.
    #[cfg_attr(trust_verify, trust::skip)]
    #[must_use]
    pub fn suffix(mut self, suffix: &str) -> Self {
        self.suffix = Some(suffix.to_owned());
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
        let dir = if self.heard {
            outside_quiet(dir.as_ref())
        } else {
            placed(dir.as_ref())
        };
        TempDir::made_in_with(&dir, || {
            let mut name = unique_name(self.prefix_str());
            if let Some(suffix) = &self.suffix {
                name.push_str(suffix);
            }
            name
        })
    }
}

/// Create a temporary directory in the system temp directory (in its [`QUIET`] folder on macOS).
/// # Errors
/// Returns an error if the directory cannot be created.
pub fn tempdir() -> io::Result<TempDir> {
    TempDir::new()
}

/// Open a private anonymous read/write file. Its temporary name and directory
/// are removed before return; the returned description keeps the file alive.
///
/// # Errors
/// Creating or unlinking the private file fails. Non-Unix targets return
/// [`io::ErrorKind::Unsupported`] before examining temporary storage.
pub fn tempfile() -> io::Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let directory = tempdir()?;
        let path = directory.path().join("file");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        fs::remove_file(&path)?;
        fs::remove_dir(directory.path())?;
        drop(directory);
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(all(test, unix))]
#[test]
fn anonymous_file_is_private_unlinked_and_preserves_binary_io() {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    use std::os::unix::fs::MetadataExt as _;
    let mut file = tempfile().unwrap();
    let metadata = file.metadata().unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.nlink(), 0);
    assert_eq!(metadata.mode() & 0o077, 0);
    let bytes = [0, 255, b'\n', 1];
    file.write_all(&bytes).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut observed = Vec::new();
    file.read_to_end(&mut observed).unwrap();
    assert_eq!(observed, bytes);
}

/// Clone the directory `src` to the new name `dst`, each file and directory under it with its
/// mode: on APFS one call whatever the tree holds, every block shared until a side writes it.
/// # Errors
/// `dst` exists, its parent does not, the two are on different volumes, the volume cannot clone,
/// or this is not macOS (`Unsupported`).
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
pub fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    /// `<sys/clonefile.h>`: do not follow a link at the source.
    const CLONE_NOFOLLOW: u32 = 0x0001;

    unsafe extern "C" {
        /// `<sys/clonefile.h>`: `int clonefile(const char *src, const char *dst, uint32_t flags)`.
        fn clonefile(src: *const std::ffi::c_char, dst: *const std::ffi::c_char, flags: u32)
        -> i32;
    }

    let c_path = |path: &Path| {
        std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a path holds a NUL"))
    };
    let (from, to) = (c_path(src)?, c_path(dst)?);
    // SAFETY: both pointers are NUL-terminated strings that outlive the call, which reads them
    // and writes nothing of ours.
    if unsafe { clonefile(from.as_ptr(), to.as_ptr(), CLONE_NOFOLLOW) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// [`clone_tree`] off macOS, where no call clones a tree.
/// # Errors
/// Always (`Unsupported`).
#[cfg(not(target_os = "macos"))]
pub fn clone_tree(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "only APFS clones a directory tree",
    ))
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
/// macOS assesses the first exec of each file, so a test pays one assessment per text, not one per
/// stand-in. Nothing here removes that file's one name, and each call marks it used (`used`), so
/// macOS's temporary-file cleaners leave it while stand-ins are laid from it (a stand-in running
/// does not keep it). macOS reads an exec's file by name to assess it, and a file with several
/// names (hard links, one per stand-in) need not be named by the link an exec ran it through: on
/// 27.0.1, once one fixture had opened its link, an exec through another fixture's left the kernel
/// naming the file, by its id, by the first fixture's link. A fixture removing its own (its
/// directory going at its end) could so leave the assessment of another fixture's exec reading a
/// name that was gone; syspolicyd then ended that stand-in as malware (`killed by signal 9`, an
/// exec security-policy exit). A symlink is no name of the file: an exec through it runs the file
/// by its one name, and `$0` is still the stand-in's own path, so a body finds what lies beside it.
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
/// `shared` names from then on, either one marked [`used`]; `None`, and `text` unnamed, when the
/// cache cannot be used.
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
            return Some(used(file.clone()));
        }
        shared.remove(text);
    }
    let file = cached_in(me, tmp, text)?;
    shared.insert(text.to_owned(), file.clone());
    Some(used(file))
}

/// `file`, its access time made now where it can be. macOS's temporary-file cleaners take a
/// slot by its access time (dirhelper, in `/var/folders`, one whose access time is more than 3
/// days old; tmp_cleaner, in `/tmp`, one whose access, modification and change times all are),
/// and on APFS a read or a run of an assessed file leaves an access time already past its
/// modification time as it is (measured 2026-10-28 on macOS 27.0). So neither `holds` reading
/// a slot nor a stand-in running keeps it: only this mark does, and a slot no call has marked
/// for 3 days is the cleaners' even while stand-ins laid from it run. One removed shows through
/// every symlink laid to it.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn used(file: PathBuf) -> PathBuf {
    if let Ok(open) = fs::File::open(&file) {
        let _ = open.set_times(fs::FileTimes::new().set_accessed(std::time::SystemTime::now()));
    }
    file
}

/// Whether `file` is a slot a stand-in of `text` may run: a regular file of `me`'s (not a link
/// to one), mode 0555, holding exactly `text`.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn holds(me: u32, file: &Path, text: &str) -> bool {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let want = text.len() as u64;
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    if !no_link_no_wait(&mut opts) && !fs::symlink_metadata(file).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(opened) = opts.open(file) else {
        return false;
    };
    let Ok(meta) = opened.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.uid() != me || meta.mode() & 0o7777 != 0o555 || meta.len() != want {
        return false;
    }
    let mut read = Vec::with_capacity(text.len().saturating_add(1));
    opened
        .take(want.saturating_add(1))
        .read_to_end(&mut read)
        .is_ok()
        && read == text.as_bytes()
}

/// Set `opts` to open a file not through a link at its last name and without waiting on a FIFO
/// for a writer (`O_NOFOLLOW | O_NONBLOCK`, by macOS's and Linux's values, this crate depending
/// on nothing), and say whether it did: elsewhere a caller looks first that the name is a
/// regular file, no link ([`holds`], `made_by_a_child`).
#[cfg(unix)]
fn no_link_no_wait(opts: &mut fs::OpenOptions) -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use std::os::unix::fs::OpenOptionsExt as _;
    #[cfg(target_os = "macos")]
    opts.custom_flags(0x0004 | 0x0100);
    #[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
    opts.custom_flags(0o100_000 | 0o4000);
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "aarch64", target_arch = "arm"))
    ))]
    opts.custom_flags(0o400_000 | 0o4000);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = opts;
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// The file holding `text` in user `me`'s stand-in cache in `tmp`'s quiet folder ([`quiet_in`]),
/// made when none of its slots holds it yet; `None` when the cache cannot be used.
/// [`shared_stand_in`]'s `me` is this process's effective uid. The cache folder is used only
/// when it is a directory, not a link to one, of `me`'s with no permission bits for group or
/// others, so another user's folder of its name (in a shared `/tmp`, made before this user's) is
/// refused.
///
/// The cache holds one file per text (a slot more only where a write was cut short or two
/// first writers raced), so it stays as small as the set of texts, and nothing here renames or
/// links a slot, or removes one written whole (macOS's cleaners take a slot no call has marked
/// [`used`] for 3 days; off Apple a write whose child fails removes what it made). A file is
/// made under its own name with no exec bit, written whole, and only then made mode 0555,
/// through a descriptor of the file made and never by path ([`slot_made`]; off Apple a child
/// process writes it and has closed it first). A slot is used only when it is a
/// regular file of `me`'s, mode 0555, holding exactly `text`, so no process runs one still
/// being written (an exec of a file open for writing is `ETXTBSY` on Linux), and a slot that is
/// not becomes the next slot's turn.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn cached_in(me: u32, tmp: &Path, text: &str) -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
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
        match slot_made(&file, text) {
            Ok(()) => return Some(file),
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

/// The slot `file` made, holding `text`, written and closed by this process, then mode 0555
/// through the descriptor it made it with; an `AlreadyExists` error when `file` exists, and
/// another when it cannot be written or its mode set (left 0600, which no use of the cache
/// takes).
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn slot_made(file: &Path, text: &str) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut made = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(file)?;
    made.write_all(text.as_bytes())?;
    made.set_permissions(fs::Permissions::from_mode(0o555))
}

/// The slot `file` made, holding `text`, written by a child process, then mode 0555 through the
/// descriptor it was checked by ([`written_by`]), never by path; an `AlreadyExists` error when a
/// name is at `file` that this call did not make (one there before, or a first writer's that
/// raced this one), and another error, with nothing this call made left at `file`, when the
/// child fails or the mode cannot be set.
#[cfg(all(unix, not(target_vendor = "apple")))]
#[cfg_attr(trust_verify, trust::skip)]
fn slot_made(file: &Path, text: &str) -> io::Result<()> {
    written_by(file, text.as_bytes(), 0o555, WRITES_ITS_INPUT)
}

/// The line [`made_by_a_child`]'s `sh` runs: `$1` opened on descriptor 3 under `set -C`, `made`
/// said on stdout once it is open, then `cat` writing its stdin there, a file readable by this
/// user alone (`umask 077`). `set -C` (POSIX's noclobber; dash, bash and BusyBox's `sh` alike)
/// makes a new file, with `O_EXCL`, where `$1` names nothing, and refuses a regular file, a
/// link to one and a link to nothing; a file of another type, or a link to one (a device, a
/// FIFO), it opens as it is, with no `O_EXCL` and no truncation.
#[cfg(all(unix, not(target_vendor = "apple")))]
const WRITES_ITS_INPUT: &str = "set -C; umask 077; { echo made; exec cat >&3; } 3>\"$1\"";

/// Make `path` (it must not exist) holding `body`, then mode `mode`, with a child process
/// writing it, so no descriptor of this process's ever has `path` open for writing: off Apple,
/// the writer of a file a test runs (`stand_in`'s, the stand-in cache's, and aterm-verify's
/// `standin` where that cache cannot be used).
///
/// Linux refuses to exec a file anything holds open for writing (`ETXTBSY`), and a child
/// another thread forks while this process holds a file open keeps that descriptor until its
/// own exec (close-on-exec closes it only then), tens to hundreds of milliseconds under load.
/// A program refused so is not always told: a zsh runs the next match on `PATH` instead. The
/// child here opens `path` after its fork and has exited before this returns, so once `path`
/// is made no process holds a writer of it. Its mode is set through a descriptor this process
/// opens to read, never to write, on the file the child made ([`made_by_a_child`]), so never
/// through a link.
///
/// Test support that aterm-verify's `standin` reaches in every profile it builds in, so it is
/// not gated by profile; no shipped code calls it.
///
/// # Errors
/// `AlreadyExists` when a name is at `path` that this call did not make, which is left as it
/// is: one there before, or one put there after this call looked (another writer's file, which
/// `set -C` refuses, or another type of file or a link to one, which it opens and the child
/// writes into). Another error when `path`'s directory refuses a file, the child does not start
/// or fails, or the mode cannot be set; then nothing this call made is left at `path`.
#[cfg(all(unix, not(target_vendor = "apple")))]
#[cfg_attr(trust_verify, trust::skip)]
#[doc(hidden)]
pub fn written_by_a_child(path: &Path, body: &[u8], mode: u32) -> io::Result<()> {
    written_by(path, body, mode, WRITES_ITS_INPUT)
}

/// [`written_by_a_child`], its child's `sh` running `line` ([`WRITES_ITS_INPUT`]).
#[cfg(all(unix, not(target_vendor = "apple")))]
#[cfg_attr(trust_verify, trust::skip)]
fn written_by(path: &Path, body: &[u8], mode: u32, line: &str) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let (made, meta) = made_by_a_child(path, body, line)?;
    let set = made.set_permissions(fs::Permissions::from_mode(mode));
    if set.is_err()
        && fs::symlink_metadata(path).is_ok_and(|m| (m.dev(), m.ino()) == (meta.dev(), meta.ino()))
    {
        let _ = fs::remove_file(path);
    }
    set
}

/// Make `path` (it must not exist) holding `body`, mode 0600, with a child process's `sh`
/// running `line` ([`WRITES_ITS_INPUT`]) on it; the file made, open to read, never to write, and
/// its metadata.
///
/// `body` reaches the child as its stdin, a file this process writes under a hidden name of its
/// own beside `path`, unnamed before the child starts (that file is never run, so a fork holding
/// it open costs nothing); so an error of `path`'s directory (`PermissionDenied`, `NotFound`) is
/// that file's. A look first refuses any name at `path`, a link whatever it leads to included,
/// so the child's `set -C` opens a file there that it did not make only when one is put there
/// after that look, by whoever may write `path`'s directory. What the child wrote is then
/// opened without following a link ([`no_link_no_wait`]; on a Unix whose flags this crate does
/// not know, after a look that it is a regular file) and taken only as a regular file of this
/// user's, of one name, `body`'s length: what `set -C` opens and is not, or a link, is no file
/// this call made.
/// The child says it opened `path` (`made`) before it writes: when it then fails, a regular file
/// at `path` is the one it made, since `set -C` opens a regular file only when it makes it, and
/// is removed.
///
/// An `AlreadyExists` error when a name is at `path` that this call did not make, left as it
/// is; another error, with nothing this call made left at `path`, when the child does not start
/// or fails.
#[cfg(all(unix, not(target_vendor = "apple")))]
#[cfg_attr(trust_verify, trust::skip)]
fn made_by_a_child(path: &Path, body: &[u8], line: &str) -> io::Result<(fs::File, fs::Metadata)> {
    use std::io::{Seek as _, Write as _};
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let not_made = |what: &str| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{}: {what}", path.display()),
        )
    };
    if fs::symlink_metadata(path).is_ok() {
        return Err(not_made("exists"));
    }
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let staged_at = dir.join(unique_name(".stand-in-body."));
    let mut staged = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged_at)?;
    fs::remove_file(&staged_at)?;
    staged.write_all(body)?;
    staged.rewind()?;
    #[expect(
        clippy::disallowed_types,
        reason = "aterm-tempfile depends on nothing, so not on spawnfd: one `sh` writing a \
                  stand-in's bytes, its stdin a file of this process's and its stdout and \
                  stderr pipes to this one"
    )]
    let mut sh = std::process::Command::new("/bin/sh");
    let out = sh
        .args(["-c", line, "sh"])
        .arg(path)
        .stdin(staged)
        .output()?;
    let named = fs::symlink_metadata(path);
    if !out.status.success() {
        let ended = format!(
            "the `sh` writing it ended with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
        if out.stdout == b"made\n" && named.as_ref().is_ok_and(fs::Metadata::is_file) {
            let _ = fs::remove_file(path);
        } else if named.is_ok() {
            return Err(not_made(&ended));
        }
        return Err(io::Error::other(format!("{}: {ended}", path.display())));
    }
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    if !no_link_no_wait(&mut opts) && !named.is_ok_and(|m| m.is_file()) {
        return Err(not_made("not the file its writer made"));
    }
    let made = opts
        .open(path)
        .map_err(|e| not_made(&format!("not the file its writer made ({e})")))?;
    let meta = made.metadata()?;
    if !meta.is_file()
        || meta.uid() != euid()
        || meta.nlink() != 1
        || meta.len() != body.len() as u64
    {
        return Err(not_made("not the file its writer made"));
    }
    Ok((made, meta))
}

/// Lay an executable stand-in at `path` holding `body`, mode 0555, replacing whatever is there by rename.
///
/// On macOS `path` is a symlink to the one file of `body` this user's stand-ins share
/// ([`shared_stand_in`]), made at a name of its own beside `path` and renamed over it. Off
/// macOS, and where that cache cannot be used, `path` is a file of its own holding `body`; off
/// Apple a child process writes that file (`written_by_a_child`), so no fork of this process
/// holds it open for writing when it runs. Either way `path` reads and runs as `body`, and `$0`
/// in it is `path`. A caller that needs a regular file of its own at `path` (one a program
/// judges by its type or by where it resolves) writes that file itself.
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
#[cfg(all(
    unix,
    target_vendor = "apple",
    any(test, debug_assertions, feature = "test-support")
))]
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

/// Create `path` (it must not exist) holding `body`, written by a child process, then make it
/// mode 0555 ([`written_by_a_child`], which leaves nothing it made behind an error and nothing
/// it did not make removed).
#[cfg(all(
    unix,
    not(target_vendor = "apple"),
    any(test, debug_assertions, feature = "test-support")
))]
fn write_read_only(path: &Path, body: &[u8]) -> io::Result<()> {
    written_by_a_child(path, body, 0o555)
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

    /// NEGATIVE CONTROL: `hex_digit` clamps nibbles `>= 16` to `'f'` while mapping `0..=15`
    /// bijectively, `unique_name` produces distinct `<prefix><pid>_<16 lowercase hex>_<counter>`
    /// strings on consecutive calls, and `encode_temp_seed_le` / `decode_temp_seed_le` roundtrip
    /// boundary values without field aliasing.
    #[test]
    fn unique_name_hex_digit_and_seed_codec_roundtrip() {
        let digits: String = (0..16).map(hex_digit).collect();
        assert_eq!(digits, "0123456789abcdef");
        assert_eq!(hex_digit(16), 'f');
        assert_eq!(hex_digit(u64::MAX), 'f');

        let first = unique_name("pfx_");
        let second = unique_name("pfx_");
        assert_ne!(first, second);
        let pid_prefix = format!("pfx_{}_", std::process::id());
        for candidate in [&first, &second] {
            let rest = candidate
                .strip_prefix(&pid_prefix)
                .expect("unique_name must start with <prefix><pid>_");
            let (hex_part, counter_part) = rest
                .split_once('_')
                .expect("unique_name must have <hex16>_<counter>");
            assert_eq!(hex_part.len(), 16);
            assert!(
                hex_part
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
            assert!(counter_part.parse::<u64>().is_ok());
        }

        for seed in [
            (0u64, 0u32, 0u32),
            (0x0123_4567_89ab_cdef, 0x89ab_cdef, 0xfedc_ba98),
            (u64::MAX, u32::MAX, u32::MAX),
        ] {
            let wire = encode_temp_seed_le(seed.0, seed.1, seed.2);
            assert_eq!(decode_temp_seed_le(wire), seed);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_cache_slots_hash_is_aterm_hashs_fold() {
        let inputs: [&[u8]; 4] = [b"", b"a", b"foobar", &[0, 0xff, 0x80, 0x7f]];
        for bytes in inputs {
            assert_eq!(fnv1a(bytes), aterm_hash::fnv1a_64(bytes), "{bytes:02x?}");
        }
    }

    /// NEGATIVE CONTROL (2026-11-05): with [`clone_tree`] linking `dst` to `src` instead of cloning
    /// it, a write to the clone reaches the source (`left: "changed"`, `right: "first"`).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_cloned_tree_holds_what_its_source_held_and_shares_nothing_after() {
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
        let dir = tempdir().unwrap();
        let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
        fs::DirBuilder::new().mode(0o700).create(&src).unwrap();
        fs::DirBuilder::new()
            .mode(0o711)
            .create(src.join("sub"))
            .unwrap();
        // Creation modes pass through the runner's umask; seed the exact modes to preserve.
        fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o711)).unwrap();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o640)
            .open(src.join("sub/file"))
            .unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o640))
            .unwrap();
        io::Write::write_all(&mut file, b"first").unwrap();
        drop(file);

        match clone_tree(&src, &dst) {
            // A scratch volume that cannot clone has nothing to judge here.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
            cloned => cloned.unwrap(),
        }

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&dst), 0o700);
        assert_eq!(mode(&dst.join("sub")), 0o711);
        assert_eq!(mode(&dst.join("sub/file")), 0o640);
        assert_eq!(fs::read(dst.join("sub/file")).unwrap(), b"first");

        fs::write(dst.join("sub/file"), b"changed").unwrap();
        assert_eq!(
            String::from_utf8(fs::read(src.join("sub/file")).unwrap()).unwrap(),
            "first"
        );
        assert_eq!(
            clone_tree(&src, &dst).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }

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
    fn a_suffix_stays_in_the_name_and_keeps_directory_cleanup() {
        let root = tempdir().expect("root");
        let parent = root.path().join("inside");
        fs::create_dir(&parent).expect("parent");
        let path;
        {
            let dir = Builder::new()
                .prefix("scratch_")
                .suffix(".noindex")
                .tempdir_in(&parent)
                .expect("a suffixed directory");
            path = dir.path().to_path_buf();
            assert_eq!(path.parent(), Some(parent.as_path()));
            let name = path.file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with("scratch_") && name.ends_with(".noindex"));
            fs::write(path.join("artifact"), b"owned").expect("artifact");
        }
        assert!(!path.exists());
        for suffix in ["/../outside", "/nested/child", "/", ".bad\0"] {
            let error = Builder::new()
                .suffix(suffix)
                .tempdir_in(&parent)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{suffix:?}");
        }
        #[cfg(windows)]
        for suffix in [r"\..\outside", r"\nested\child", r"\", ".", " "] {
            let error = Builder::new()
                .suffix(suffix)
                .tempdir_in(&parent)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{suffix:?}");
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(parent).unwrap().count(), 0);
    }

    #[test]
    fn a_prefix_cannot_escape_the_requested_parent() {
        let root = tempdir().expect("root");
        let parent = root.path().join("inside");
        fs::create_dir(&parent).expect("parent");
        let absolute = root.path().join("outside_");
        for prefix in [
            "../outside_",
            "nested/child_",
            "nul\0_",
            absolute.to_str().unwrap(),
        ] {
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

    /// A SLOT HANDED OUT LOOKS JUST USED. macOS's temporary-file cleaners take a file by its
    /// access time, which `holds` reading a slot does not move on APFS (the slot is aged with
    /// both times equal, which a read there leaves as they are): the slot each call hands out,
    /// whether `cached_in` found it (made by an earlier process) or the memo named it, is handed
    /// out with its access time made now. NEGATIVE CONTROL: only that: the slot handed out keeps
    /// its modification time (it is not written), and a slot passed over keeps its age. That slot
    /// is one a writer left mid-write (0600), which `holds` refuses without reading, aged to an
    /// access time before its modification time, which a read would move on APFS as under
    /// Linux's relatime, so the control fails on a mark of it, not on a filesystem's read policy.
    #[cfg(unix)]
    #[test]
    fn a_slot_handed_out_is_marked_used_and_one_passed_over_is_not() {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        use std::time::{Duration, SystemTime};
        const DAY: Duration = Duration::from_secs(24 * 60 * 60);
        const CLEANED: Duration = Duration::from_secs(3 * 24 * 60 * 60);
        let me = euid();
        let tmp = tempdir().expect("scratch");
        let text = "#!/bin/sh\nexit 8\n";
        let cache = quiet_in(tmp.path()).join(format!("{STAND_IN_CACHE}{me}"));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&cache)
            .expect("the cache");
        let other = cache.join(format!("{:016x}-0", fnv1a(text.as_bytes())));
        fs::write(&other, "#!/bin/sh\nexit").expect("lay a slot");
        fs::set_permissions(&other, fs::Permissions::from_mode(0o600)).expect("chmod a slot");
        let slot = cached_in(me, tmp.path(), text).expect("the text's slot");
        let unused = SystemTime::now() - CLEANED - DAY;
        let age = |file: &Path, accessed: SystemTime| {
            let times = fs::FileTimes::new()
                .set_accessed(accessed)
                .set_modified(unused);
            fs::File::open(file)
                .and_then(|f| f.set_times(times))
                .expect("aged");
        };
        let since = |file: &Path, of: fn(&fs::Metadata) -> io::Result<SystemTime>| {
            let at = fs::metadata(file).and_then(|m| of(&m)).expect("its times");
            SystemTime::now().duration_since(at).unwrap_or_default()
        };
        let mut shared = BTreeMap::new();
        for named in ["found in the cache", "named by the memo"] {
            age(&slot, unused);
            age(&other, unused - DAY);
            let got = shared_in(&mut shared, me, tmp.path(), text);
            assert_eq!(got.as_ref(), Some(&slot), "{named}");
            assert!(
                since(&slot, fs::Metadata::accessed) < CLEANED,
                "{} handed out ({named}) still looks unused",
                slot.display()
            );
            assert!(
                since(&slot, fs::Metadata::modified) >= CLEANED,
                "{} was written",
                slot.display()
            );
            assert!(
                since(&other, fs::Metadata::accessed) >= CLEANED,
                "{} passed over ({named}) was marked used",
                other.display()
            );
        }
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

    /// OFF APPLE A CHILD WRITER REMOVES ONLY WHAT IT MADE. A file another writer makes at the
    /// name after the writer looked (here the child's own line does, ahead of its open) is
    /// refused by `set -C` and left whole, with `AlreadyExists` (`write_read_only` and the
    /// stand-in cache both write through this). A file the child made and then failed to write
    /// is removed, with the child's error. NEGATIVE CONTROL: a file the child makes and writes
    /// whole is kept, holding the body, of one name, in the mode asked, and no writer leaves
    /// the body it staged.
    #[cfg(all(unix, not(target_vendor = "apple")))]
    #[test]
    fn a_child_writer_removes_only_what_it_made() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempdir().expect("a fixture");
        let theirs = dir.path().join("theirs");
        let raced = format!("echo theirs >\"$1\"; {WRITES_ITS_INPUT}");
        let refused = written_by(&theirs, b"mine\n", 0o555, &raced);
        assert_eq!(
            refused.map_err(|e| e.kind()),
            Err(io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            fs::read_to_string(&theirs).ok().as_deref(),
            Some("theirs\n"),
            "another writer's file is left whole"
        );
        let cut = dir.path().join("cut");
        let fails = "set -C; umask 077; { echo made; echo part >&3; exit 1; } 3>\"$1\"";
        let failed = written_by(&cut, b"whole\n", 0o555, fails)
            .expect_err("the child failed")
            .kind();
        assert_ne!(failed, io::ErrorKind::AlreadyExists);
        assert!(
            fs::symlink_metadata(&cut).is_err(),
            "{} is left part-written",
            cut.display()
        );
        let made = dir.path().join("made");
        written_by_a_child(&made, b"whole\n", 0o555).expect("written whole");
        let meta = fs::symlink_metadata(&made).expect("made");
        assert!(meta.is_file() && meta.nlink() == 1, "{meta:?}");
        assert_eq!(meta.mode() & 0o7777, 0o555);
        assert_eq!(fs::read(&made).ok().as_deref(), Some(&b"whole\n"[..]));
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .expect("list")
            .map(|e| e.expect("entry").file_name().into_string().expect("UTF-8"))
            .collect();
        names.sort();
        assert_eq!(names, ["made", "theirs"]);
    }

    /// OFF APPLE A CHILD WRITER SETS A MODE ONLY ON THE FILE ITS CHILD MADE. A link to a FIFO
    /// put at the name after the writer looked (here by the child's own line, ahead of its
    /// open) is opened by `set -C`, and the child writes into the FIFO; the writer then refuses
    /// the name as no file it made (`AlreadyExists`) and leaves the link, and the FIFO's mode,
    /// as they were. NEGATIVE CONTROL: the bytes reached the FIFO, so the child did write
    /// through the link.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_writers_mode_is_never_set_through_a_link() {
        use std::io::Read as _;
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempdir().expect("a fixture");
        let mode = |path: &Path| fs::symlink_metadata(path).map(|m| m.mode() & 0o7777).ok();
        let fifo = dir.path().join("fifo");
        let made = aterm_uds::deadline::output_within(
            aterm_uds::spawnfd::command("mkfifo")
                .args(["-m", "600"])
                .arg(&fifo),
            MUST_HAPPEN,
        )
        .expect("mkfifo ran");
        assert!(made.status.success(), "mkfifo: {made:?}");
        assert_eq!(mode(&fifo), Some(0o600));
        let mut opts = fs::OpenOptions::new();
        opts.read(true);
        assert!(no_link_no_wait(&mut opts), "a reader waits for no writer");
        let mut reader = opts.open(&fifo).expect("the FIFO's reader");
        let tool = dir.path().join("tool");
        let steered = format!("ln -s fifo \"$1\"; {WRITES_ITS_INPUT}");
        let refused = written_by(&tool, b"body\n", 0o755, &steered);
        let mut got = Vec::new();
        reader.read_to_end(&mut got).expect("what the child wrote");
        assert_eq!(got, b"body\n", "the child wrote through the link");
        assert_eq!(mode(&fifo), Some(0o600), "the FIFO's mode was set");
        assert!(
            fs::symlink_metadata(&tool).is_ok_and(|m| m.file_type().is_symlink()),
            "the link is left"
        );
        assert_eq!(
            refused.map_err(|e| e.kind()),
            Err(io::ErrorKind::AlreadyExists)
        );
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
        let sep = std::path::MAIN_SEPARATOR;
        assert!(
            made.to_str()
                .is_some_and(|p| p.contains(&format!("{sep}.ubd{sep}"))),
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
        assert_eq!(outside_quiet(Path::new(".ubd/x")), Path::new("."));
        assert_eq!(outside_quiet(Path::new(".ubd")), Path::new("."));
        assert_eq!(outside_quiet(Path::new("")), Path::new(""));
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
