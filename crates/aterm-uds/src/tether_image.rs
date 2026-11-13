// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A retained helper image is never reclaimed during the boot that may assess it.
//! Parent death is not execution proof. Only a different kernel boot clears that uncertainty.

use std::fs::File;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, FileExt as _, OpenOptionsExt as _};
#[cfg(target_os = "macos")]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::private_file::{PrivateDirectory, check_private_dir, create_private_dir};

#[cfg(target_os = "linux")]
#[path = "tether_image_linux.rs"]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::{Retained, Source};

#[cfg(target_os = "macos")]
pub(super) struct Source {
    hash: [u8; 20],
    bundle: Option<(PathBuf, PathBuf)>,
}

#[cfg(target_os = "macos")]
impl Source {
    pub(super) fn read(file: &File) -> io::Result<Self> {
        let hash = i32::try_from(std::process::id())
            .ok()
            .and_then(crate::process::read_code_directory_hash)
            .ok_or_else(|| io::Error::other("loaded code hash unavailable"))?;
        Self::from_hash(file, hash)
    }

    pub(super) fn selected(file: &File) -> io::Result<Self> {
        let hash = crate::lan::install::CodeDirectories::of_held(file)?.selected_native_hash()?;
        Self::from_hash(file, hash)
    }

    fn from_hash(file: &File, hash: [u8; 20]) -> io::Result<Self> {
        let bound = crate::lan::install::CodeDirectories::of_held(file)?
            .bundle_bound(hash)
            .ok_or_else(|| io::Error::other("native CodeDirectory bundle binding unavailable"))?;
        let bundle = if bound {
            use std::os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _};
            let mut bytes = [0_u8; libc::PATH_MAX as usize];
            // SAFETY: a live descriptor and PATH_MAX writable bytes; F_GETPATH writes
            // a terminated path, including the new name after an app bundle rename.
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, bytes.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let end = bytes
                .iter()
                .position(|b| *b == 0)
                .ok_or_else(|| io::Error::other("image path unterminated"))?;
            let exe = Path::new(std::ffi::OsStr::from_bytes(&bytes[..end]));
            let macos = exe
                .parent()
                .filter(|p| p.file_name().is_some_and(|s| s == "MacOS"));
            let contents = macos
                .and_then(Path::parent)
                .filter(|p| p.file_name().is_some_and(|s| s == "Contents"));
            let root = contents
                .and_then(Path::parent)
                .filter(|p| p.extension().is_some_and(|s| s == "app"))
                .ok_or_else(|| io::Error::other("bound image has no app bundle"))?;
            Some((
                root.to_path_buf(),
                exe.strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_path_buf(),
            ))
        } else {
            None
        };
        Ok(Self { hash, bundle })
    }

    pub(super) fn bundled(&self) -> bool {
        self.bundle.is_some()
    }

    pub(super) fn retain(&self, file: &File, name: &std::ffi::OsStr) -> io::Result<Retained> {
        let root = cache_root()?;
        let boot = crate::process::boot_id()
            .ok_or_else(|| io::Error::other("boot identity unavailable"))?;
        if let Some((source, relative)) = &self.bundle {
            use std::fmt::Write as _;
            let current = prepare(&root, &boot)?;
            let mut key = String::with_capacity(40);
            for b in self.hash {
                let _ = write!(key, "{b:02x}");
            }
            let directory = current.join(format!("bundle-{key}.noindex"));
            let path = directory.join("retained.app").join(relative);
            if directory.try_exists()? {
                check_private_dir(&directory)?;
                return Retained::read(path, self.hash);
            }
            let current_dir = PrivateDirectory::open(&current)?;
            let _lock = current_dir.lock(
                &format!("bundle-{key}.lock"),
                Duration::ZERO,
                "bundle retention in progress",
            )?;
            if directory.try_exists()? {
                check_private_dir(&directory)?;
                return Retained::read(path, self.hash);
            }
            let stage = aterm_tempfile::Builder::new()
                .prefix("bundle-")
                .suffix(".noindex")
                .tempdir_in(&current)?;
            aterm_tempfile::clone_tree(source, &stage.path().join("retained.app"))?;
            Retained::read(stage.path().join("retained.app").join(relative), self.hash)?;
            // This immutable name is visible to every frontend only after the complete
            // bundle exists. Once published, no process removes it during this boot.
            std::fs::rename(stage.path(), &directory)?;
            let _ = stage.keep();
            Retained::read(path, self.hash)
        } else {
            Retained::in_cache(file, name, self.hash, &root, &boot)
        }
    }
}

fn prepare(root: &Path, boot: &str) -> io::Result<PathBuf> {
    if !boot_name(boot) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    create_private_dir(root)?;
    let _ = sweep(root, boot);
    let current = root.join(boot);
    create_private_dir(&current)?;
    Ok(current)
}

#[cfg(target_os = "macos")]
pub(super) struct Retained {
    path: PathBuf,
    identity: (u64, u64),
    hash: [u8; 20],
}

#[cfg(target_os = "macos")]
impl Retained {
    pub(super) fn path(&self) -> Option<&Path> {
        let meta = std::fs::symlink_metadata(&self.path).ok()?;
        (meta.is_file()
            && (meta.dev(), meta.ino()) == self.identity
            && crate::lan::install::CodeDirectories::of_file(&self.path)
                .ok()?
                .matches_loaded_hash(self.hash))
        .then_some(&self.path)
    }

    fn read(path: PathBuf, hash: [u8; 20]) -> io::Result<Self> {
        let meta = std::fs::symlink_metadata(&path)?;
        let retained = Self {
            path,
            identity: (meta.dev(), meta.ino()),
            hash,
        };
        retained
            .path()
            .ok_or_else(|| io::Error::other("retained image differs from loaded code"))?;
        Ok(retained)
    }

    fn in_cache(
        source: &File,
        name: &std::ffi::OsStr,
        hash: [u8; 20],
        root: &Path,
        boot: &str,
    ) -> io::Result<Self> {
        if !boot_name(boot) || Path::new(name).file_name() != Some(name) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let current = prepare(root, boot)?;
        use std::fmt::Write as _;
        let mut key = String::with_capacity(40);
        for b in hash {
            let _ = write!(key, "{b:02x}");
        }
        let directory = current.join(format!("image-{key}.noindex"));
        create_private_dir(&directory)?;
        let path = directory.join(name);
        if path.try_exists()? {
            return Self::read(path, hash);
        }
        let owned = PrivateDirectory::open(&directory)?;
        let current_dir = PrivateDirectory::open(&current)?;
        let _lock = current_dir.lock(
            &format!("image-{key}.lock"),
            Duration::from_secs(60),
            "standalone image retention in progress",
        )?;
        if path.try_exists()? {
            return Self::read(path, hash);
        }
        let stage = aterm_tempfile::Builder::new()
            .prefix("image-")
            .suffix(".noindex")
            .tempdir_in(&directory)?;
        let staged = stage.path().join(name);
        copy_image(source, &staged)?;
        if !crate::lan::install::CodeDirectories::of_file(&staged)?.matches_loaded_hash(hash) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retained image differs from loaded code",
            ));
        }
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o500))?;
        // No child has been offered this path before publication. From this point onward,
        // even an owner killed before receiving an execution receipt cannot remove it.
        if !owned.is_at(&directory)? {
            return Err(io::Error::other("standalone image directory changed"));
        }
        std::fs::rename(staged, &path)?;
        Self::read(path, hash)
    }
}

fn copy_image(source: &File, path: &Path) -> io::Result<()> {
    const MAX_IMAGE: u64 = 2 << 30;
    let size = source.metadata()?.len();
    if size == 0 || size > MAX_IMAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "executable size outside retained image bound",
        ));
    }
    if !crate::regular_file::clone_held(source, path)? {
        let mut target = File::options()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(path)?;
        let mut bytes = [0_u8; 64 * 1024];
        let mut at = 0;
        while at < size {
            let wanted = usize::try_from((size - at).min(bytes.len() as u64)).unwrap();
            let read = source.read_at(&mut bytes[..wanted], at)?;
            if read == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            target.write_all(&bytes[..read])?;
            at += read as u64;
        }
    }
    if source.metadata()?.len() != size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "executable changed while retained",
        ));
    }
    Ok(())
}

fn boot_name(name: &str) -> bool {
    name.len() == 32
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && name.bytes().any(|b| b != b'0')
}

fn cache_root() -> io::Result<PathBuf> {
    #[cfg(any(test, debug_assertions))]
    if crate::test_binary::scratch_root().is_some() {
        if let Some(root) = TEST_ROOT.get() {
            return machine_root(root.clone());
        }
        // A test's scratch HOME and per-process roots can disappear before a helper's
        // first execution is assessed. Use an isolated persistent store rooted in the
        // account record, never the application's state or its test-root exit sweep.
        let home = crate::account::home()
            .filter(|home| home.is_absolute())
            .ok_or_else(|| io::Error::other("test image account home unavailable"))?;
        return persistent_root(home.into_os_string(), "aterm-test-tether-images");
    }
    // Temporary directories and purgeable caches cannot retain an image through a
    // delayed macOS assessment. Keep this beside the persistent application state,
    // independently of its migration and writer leases.
    persistent_root(
        std::env::var_os("HOME")
            .ok_or_else(|| io::Error::other("persistent image storage unavailable"))?,
        "aterm-tether-images",
    )
}

fn persistent_root(home: std::ffi::OsString, name: &str) -> io::Result<PathBuf> {
    let state = crate::per_user_default(None, Some(home))
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("persistent image storage unavailable"))?;
    // A first launch or an isolated frontend can have a HOME with no Library yet.
    // Make only missing ancestors; existing account directories keep their modes.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(
            state
                .parent()
                .ok_or_else(|| io::Error::other("image state parent missing"))?,
        )?;
    let root = crate::quiet_state::destination(&state.with_file_name(name));
    create_private_dir(
        root.parent()
            .ok_or_else(|| io::Error::other("image parent missing"))?,
    )?;
    machine_root(root)
}

fn machine_root(base: PathBuf) -> io::Result<PathBuf> {
    let host = crate::process::host_id()
        .ok_or_else(|| io::Error::other("image cache machine identity unavailable"))?;
    let prefix = if cfg!(target_os = "macos") {
        "darwin:"
    } else {
        "linux:"
    };
    let name = host
        .strip_prefix(prefix)
        .filter(|s| boot_name(s))
        .ok_or_else(|| io::Error::other("image cache machine identity malformed"))?;
    create_private_dir(&base)?;
    // Homes can be shared or copied. Another machine's current boot is never
    // evidence that a local assessment ended, and must stay outside our sweep.
    Ok(base.join(format!("host-{name}.noindex")))
}

fn sweep(root: &Path, current: &str) -> io::Result<()> {
    if !boot_name(current) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let directory = PrivateDirectory::open(root)?;
    // Contention postpones reclamation, not image availability. Only this boot creates files;
    // an earlier boot has no process or assessment that can still use its directories.
    let _lock = directory.lock("gc.lock", Duration::ZERO, "image cleanup in progress")?;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == current || !boot_name(name) {
            continue;
        }
        let path = entry.path();
        if check_private_dir(&path).is_ok() && directory.is_at(root)? {
            std::fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

#[cfg(any(test, debug_assertions))]
static TEST_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg(any(test, debug_assertions))]
pub(super) fn test_root(root: PathBuf) -> io::Result<()> {
    if crate::test_binary::scratch_root().is_none() || !root.is_absolute() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    TEST_ROOT
        .set(root)
        .map_err(|_| io::Error::other("test image cache already selected"))
}

#[cfg(any(test, debug_assertions))]
pub(super) fn test_sweep() -> io::Result<()> {
    TEST_ROOT
        .get()
        .ok_or_else(|| io::Error::other("test image cache not selected"))?;
    let root = cache_root()?;
    let boot =
        crate::process::boot_id().ok_or_else(|| io::Error::other("boot identity unavailable"))?;
    sweep(&root, &boot)
}

#[cfg(test)]
#[path = "tether_image_tests.rs"]
mod tests;
