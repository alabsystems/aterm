// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A loaded executable's identity, independent of later changes at its pathname.

use std::path::{Path, PathBuf};

/// The program a process loaded, retaining its kernel identity even after an update replaces
/// the file at its launch path. An unavailable kernel observation supplies no identity.
#[derive(Clone, Debug)]
pub struct LoadedImage {
    path: PathBuf,
    #[cfg(unix)]
    file: FileIdentity,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
struct ImageRead {
    meta: std::fs::Metadata,
    path: PathBuf,
}

#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn read_image(dir: &super::ProcDir, name: &std::ffi::CStr) -> Option<ImageRead> {
    // Metadata precedes its path: a zero-link observation must precede any kernel
    // deletion marker we use to recover the launch path.
    let meta = dir.open(name)?.metadata().ok()?;
    let path = dir.link(name)?;
    Some(ImageRead { meta, path })
}

#[cfg(unix)]
impl FileIdentity {
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            device: meta.dev(),
            inode: meta.ino(),
        }
    }

    fn matches(self, meta: &std::fs::Metadata) -> bool {
        let other = Self::of(meta);
        self.inode == other.inode && self.device == other.device
    }
}

impl LoadedImage {
    /// Read `pid`'s loaded image from the kernel, not from the file currently at its pathname.
    /// Linux reads through the process's held `/proc` directory, including the exec descriptor
    /// of a binary translator. Unsupported platforms or unreadable identities return `None`.
    #[must_use]
    pub fn read(pid: i32) -> Option<Self> {
        if pid <= 0 {
            return None;
        }
        #[cfg(target_os = "macos")]
        {
            let before = super::read_identity(pid)?;
            let file = aterm_sysprobe::kproc::image_file(u32::try_from(pid).ok()?)?;
            let path = super::read_process_path(pid)?;
            (aterm_sysprobe::kproc::image_file(u32::try_from(pid).ok()?) == Some(file)
                && super::read_identity(pid) == Some(before))
            .then_some(Self {
                path,
                file: FileIdentity {
                    device: file.device,
                    inode: file.inode,
                },
            })
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let before = super::read_identity(pid)?;
            let image = Self::read_linux(Path::new("/proc"), pid)?;
            (super::read_identity(pid) == Some(before)).then_some(image)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
        {
            None
        }
    }

    /// The path the kernel names for the loaded image. It may follow a rename on macOS, where
    /// a caller monitoring a service's original launch path should retain that path separately.
    /// Only a proven deleted Linux image has its kernel ` (deleted)` marker removed; a live
    /// file whose name ends that way keeps its full name.
    #[must_use]
    pub fn launch_path(&self) -> &Path {
        &self.path
    }

    /// Whether the regular file now at `path` is this loaded image. Missing or unreadable
    /// files return `None`, so a moved-away app is not mistaken for an available replacement.
    #[must_use]
    pub fn matches_file(&self, path: &Path) -> Option<bool> {
        let meta = std::fs::metadata(path).ok()?;
        meta.is_file().then(|| self.matches_metadata(&meta))
    }

    /// Compare metadata already read from a candidate file with this immutable loaded image.
    #[must_use]
    pub fn matches_metadata(&self, meta: &std::fs::Metadata) -> bool {
        #[cfg(unix)]
        {
            meta.is_file() && self.file.matches(meta)
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            false
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
    fn read_linux(proc: &Path, pid: i32) -> Option<Self> {
        Self::read_linux_with(proc, pid, &mut read_image)
    }

    #[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
    fn read_linux_with(
        proc: &Path,
        pid: i32,
        image: &mut dyn FnMut(&super::ProcDir, &std::ffi::CStr) -> Option<ImageRead>,
    ) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let dir = super::ProcDir::of(proc, pid)?;
        let descriptor = exec_descriptor(&dir.read(c"auxv")?)?;
        let name = descriptor
            .map(|fd| std::ffi::CString::new(format!("fd/{fd}")).ok())
            .unwrap_or_else(|| Some(c"exe".to_owned()))?;
        let first = image(&dir, &name)?;
        let (path, meta) = (first.path, first.meta);
        if !meta.is_file() || !path.is_absolute() {
            return None;
        }
        let file = FileIdentity::of(&meta);
        // Keep this proof before the next read: a literal filename ending in the kernel's
        // marker may be unlinked between the final raw link and normalization.
        let first_live =
            meta.nlink() != 0 && std::fs::metadata(&path).is_ok_and(|now| file.matches(&now));
        if let Some(fd) = descriptor {
            let info = dir.read(&std::ffi::CString::new(format!("fdinfo/{fd}")).ok()?)?;
            let info = std::str::from_utf8(&info).ok()?;
            if !super::procfs::exec_opened(info)
                || super::procfs::open_inode(info) != Some(meta.ino())
            {
                return None;
            }
        }
        // Reuse of an exec descriptor, or an exec between reading the descriptor and its
        // link, must not splice a new path onto the old image's identity.
        let mut last = image(&dir, &name)?;
        if !file.matches(&last.meta) || exec_descriptor(&dir.read(c"auxv")?)? != descriptor {
            return None;
        }
        let mut marked = path.as_os_str().to_os_string();
        marked.push(" (deleted)");
        let appended = last.path.as_os_str() == marked.as_os_str();
        if appended && last.meta.nlink() != 0 {
            // Deletion may fall between the final metadata and link. One more bounded
            // metadata-before-link observation can prove that exact marker transition.
            let refreshed = image(&dir, &name)?;
            if !file.matches(&refreshed.meta)
                || refreshed.path != last.path
                || exec_descriptor(&dir.read(c"auxv")?)? != descriptor
            {
                return None;
            }
            last = refreshed;
        }
        let loaded = if appended && last.meta.nlink() == 0 {
            // Retain the first raw path verbatim: it may itself end in a literal marker.
            Self { path, file }
        } else if last.path == path {
            if first_live && last.meta.nlink() != 0 {
                Self { path, file }
            } else {
                // This metadata preceded the decisive link, so a zero link count cannot
                // accidentally strip a literal name read before the unlink.
                Self::from_opened(last.path, &last.meta)?
            }
        } else {
            return None;
        };
        if descriptor.is_some()
            && !maps_image(
                std::io::BufReader::new(dir.open(c"maps")?),
                loaded.launch_path(),
                file,
            )
        {
            return None;
        }
        Some(loaded)
    }

    #[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
    fn from_opened(path: PathBuf, meta: &std::fs::Metadata) -> Option<Self> {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;
        if !path.is_absolute() || !meta.is_file() {
            return None;
        }
        let file = FileIdentity::of(meta);
        let path = match path.as_os_str().as_bytes().strip_suffix(b" (deleted)") {
            None => path,
            Some(original) => {
                if meta.nlink() == 0 {
                    PathBuf::from(std::ffi::OsStr::from_bytes(original))
                } else if std::fs::metadata(&path).is_ok_and(|now| file.matches(&now)) {
                    // A literal suffix in a live filename is not a kernel deletion marker.
                    path
                } else {
                    // With surviving hard links and no matching full path, the suffix is
                    // ambiguous. Keep the service alive until a later observation proves it.
                    return None;
                }
            }
        };
        Some(Self { path, file })
    }
}

/// Compare a process's loaded executable with the regular file now at `path`, without
/// requiring the process's launch path. Missing files or unavailable kernel identity return
/// `None`; a node supervisor can therefore retain its existing configured-path policy.
#[must_use]
pub fn image_matches_file(pid: i32, path: &Path) -> Option<bool> {
    if pid <= 0 {
        return None;
    }
    #[cfg(unix)]
    {
        observed_match(path, || image_identity(pid))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(unix)]
fn observed_match(path: &Path, mut image: impl FnMut() -> Option<FileIdentity>) -> Option<bool> {
    let before = image()?;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || image()? != before {
        return None;
    }
    Some(before.matches(&meta))
}

#[cfg(unix)]
fn image_identity(pid: i32) -> Option<FileIdentity> {
    #[cfg(target_os = "macos")]
    {
        let before = super::read_identity(pid)?;
        let file = aterm_sysprobe::kproc::image_file(u32::try_from(pid).ok()?)?;
        (aterm_sysprobe::kproc::image_file(u32::try_from(pid).ok()?) == Some(file)
            && super::read_identity(pid) == Some(before))
        .then_some(FileIdentity {
            device: file.device,
            inode: file.inode,
        })
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let before = super::read_identity(pid)?;
        let proc = Path::new("/proc");
        let dir = super::ProcDir::of(proc, pid)?;
        let identity = match exec_descriptor(&dir.read(c"auxv")?)? {
            None => {
                let meta = dir.open(c"exe")?.metadata().ok()?;
                let after = dir.open(c"exe")?.metadata().ok()?;
                if !meta.is_file()
                    || !after.is_file()
                    || !FileIdentity::of(&meta).matches(&after)
                    || exec_descriptor(&dir.read(c"auxv")?)?.is_some()
                {
                    return None;
                }
                FileIdentity::of(&meta)
            }
            Some(_) => LoadedImage::read_linux(proc, pid)?.file,
        };
        (super::read_identity(pid) == Some(before)).then_some(identity)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    {
        let _ = pid;
        None
    }
}

/// A complete native-endian auxv distinguishes a native image from a translator whose exec
/// descriptor could not be read. In the latter case never fall back to the translator's exe.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn exec_descriptor(auxv: &[u8]) -> Option<Option<i32>> {
    exec_descriptor_words(auxv, std::mem::size_of::<usize>())
}

#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn exec_descriptor_words(auxv: &[u8], word: usize) -> Option<Option<i32>> {
    let number = |bytes: &[u8]| match word {
        4 => Some(u64::from(u32::from_ne_bytes(bytes.try_into().ok()?))),
        8 => Some(u64::from_ne_bytes(bytes.try_into().ok()?)),
        _ => None,
    };
    if !matches!(word, 4 | 8) || !auxv.len().is_multiple_of(2 * word) {
        return None;
    }
    for pair in auxv.chunks_exact(2 * word) {
        match number(&pair[..word])? {
            0 => return Some(None),
            2 => {
                return i32::try_from(number(&pair[word..])?)
                    .ok()
                    .filter(|fd| *fd >= 0)
                    .map(Some);
            }
            _ => {}
        }
    }
    None
}

/// Require the loaded inode and device as well as the path: a reused descriptor naming a
/// replacement must not borrow an executable mapping of the old file at that same pathname.
/// The kernel may append its deletion marker after the image observation; the loaded device,
/// inode and executable mapping must still match before either spelling is accepted.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn maps_image(mut maps: impl std::io::BufRead, path: &Path, image: FileIdentity) -> bool {
    use std::io::{BufRead as _, Read as _};
    use std::os::unix::ffi::OsStrExt as _;
    let device = image.device;
    let native_device = device as libc::dev_t;
    if native_device as u64 != device {
        return false;
    }
    let device_pair = (
        libc::major(native_device) as u64,
        libc::minor(native_device) as u64,
    );
    let mut shown = Vec::new();
    for &byte in path.as_os_str().as_bytes() {
        if byte == b'\n' {
            shown.extend_from_slice(b"\\012");
        } else {
            shown.push(byte);
        }
    }
    let mut row = Vec::new();
    loop {
        row.clear();
        if (&mut maps)
            .take(64 * 1024)
            .read_until(b'\n', &mut row)
            .map_or(true, |len| len == 0)
        {
            return false;
        }
        let mut rest = row.strip_suffix(b"\n").unwrap_or(&row);
        let mut fields: [&[u8]; 5] = [&[]; 5];
        for field in &mut fields {
            rest = rest.trim_ascii_start();
            let end = rest
                .iter()
                .position(u8::is_ascii_whitespace)
                .unwrap_or(rest.len());
            (*field, rest) = rest.split_at(end);
        }
        let dev = std::str::from_utf8(fields[3])
            .ok()
            .and_then(|dev| dev.split_once(':'))
            .and_then(|(a, b)| {
                Some((
                    u64::from_str_radix(a, 16).ok()?,
                    u64::from_str_radix(b, 16).ok()?,
                ))
            });
        let inode = std::str::from_utf8(fields[4])
            .ok()
            .and_then(|n| n.parse::<u64>().ok());
        if fields[1].get(2) == Some(&b'x')
            && dev == Some(device_pair)
            && inode == Some(image.inode)
            && (rest.trim_ascii_start() == shown
                || rest.trim_ascii_start().strip_suffix(b" (deleted)") == Some(shown.as_slice()))
        {
            return true;
        }
    }
}

#[cfg(all(test, unix))]
#[path = "process_image_tests.rs"]
mod tests;
