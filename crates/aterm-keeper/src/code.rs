// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keeper runs from its own copy of its code.

use std::io;
use std::path::{Path, PathBuf};

use aterm_stamp::release_version::ReleaseVersion;

use crate::identity::Identity;
use crate::wire::Birth;

/// Variable naming the executable a keeper was started as.
pub const ORIGIN_ENV: &str = "ATERM_KEEPER_ORIGIN";

/// Variable naming, across the re-exec into the copy, the file that stood at [`ORIGIN_ENV`]'s
/// path when the copy was made ([`FileStamp::encode`]).
pub const ORIGIN_STAMP_ENV: &str = "ATERM_KEEPER_ORIGIN_STAMP";

/// Which file stands at a path: its device, inode, size and modification time. A file renamed
/// over the path, and one rewritten in place, read as another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
}

impl FileStamp {
    /// The file at `path` now (links followed).
    /// # Errors
    /// `path` not reading.
    pub fn read(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(path)?;
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
        })
    }

    /// `dev:ino:size:mtime:mtime_nsec`, what [`Self::parse`] reads.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.dev, self.ino, self.size, self.mtime, self.mtime_nsec
        )
    }

    /// What [`Self::encode`] wrote, or `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.split(':');
        let stamp = Self {
            dev: parts.next()?.parse().ok()?,
            ino: parts.next()?.parse().ok()?,
            size: parts.next()?.parse().ok()?,
            mtime: parts.next()?.parse().ok()?,
            mtime_nsec: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(stamp)
    }
}

/// The executable a keeper was started as, and the file that stood at its path then: launchd
/// starts whatever stands there when the keeper exits, so a wanted restart goes only once
/// another file does, a build at least as new as the keeper's own version
/// ([`crate::core::KeeperCore::restart_when_safe`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExeImage {
    path: PathBuf,
    stamp: FileStamp,
    /// The keeper's own release version ([`Self::serving`]); `None` ranks no replacement.
    own: Option<ReleaseVersion>,
}

impl ExeImage {
    /// `path` and the file at it when the keeper started, for a keeper of no known version (see
    /// [`Self::serving`]).
    #[must_use]
    pub const fn new(path: PathBuf, stamp: FileStamp) -> Self {
        Self {
            path,
            stamp,
            own: None,
        }
    }

    /// This image for a keeper of version `version`: a replacement counts only when its bundle
    /// says it is a release at least as new ([`Self::look`]). A version that is no release
    /// version (a dev build's, a test's) ranks none.
    #[must_use]
    pub fn serving(self, version: &str) -> Self {
        Self {
            own: ReleaseVersion::parse(version),
            ..self
        }
    }

    /// `path` and the file at it now.
    /// # Errors
    /// `path` not reading.
    pub fn at(path: &Path) -> io::Result<Self> {
        Ok(Self::new(path.to_path_buf(), FileStamp::read(path)?))
    }

    /// The executable's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file that stood at [`Self::path`] when the keeper started.
    #[must_use]
    pub const fn stamp(&self) -> FileStamp {
        self.stamp
    }

    /// What stands at [`Self::path`] now, beside the file the keeper started as: another file
    /// is [`ImageLook::Replaced`](crate::core::ImageLook::Replaced) only when its bundle's
    /// version ([`bundle_version`]) is at least this keeper's ([`Self::serving`]), so a downgrade
    /// never restarts the keeper into an older build.
    #[must_use]
    pub fn look(&self) -> crate::core::ImageLook {
        use crate::core::ImageLook;
        match FileStamp::read(&self.path) {
            Ok(now) if now == self.stamp => ImageLook::Same,
            Ok(_) => {
                let path = self.path.display().to_string();
                match (bundle_version(&self.path), self.own) {
                    (Some(found), Some(own)) if found >= own => ImageLook::Replaced(path),
                    _ => ImageLook::Older(path),
                }
            }
            Err(_) => ImageLook::Unknown,
        }
    }
}

/// The largest `Info.plist` [`bundle_version`] reads.
const PLIST_MAX: u64 = 1 << 20;

/// The release version of the bundle whose executable is `exe` (`<bundle>/Contents/MacOS/<exe>`):
/// its `Contents/Info.plist`'s `CFBundleShortVersionString`. `None` for an executable outside a
/// `MacOS` directory, an `Info.plist` that does not read or is not XML, and a value that is no
/// release version.
#[must_use]
pub fn bundle_version(exe: &Path) -> Option<ReleaseVersion> {
    use std::io::Read as _;
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(macos.parent()?.join("Info.plist"))
        .ok()?
        .take(PLIST_MAX)
        .read_to_string(&mut text)
        .ok()?;
    plist_short_version(&text)
}

/// The `CFBundleShortVersionString` of an XML plist's `text`, outside its comments, as a release
/// version.
fn plist_short_version(text: &str) -> Option<ReleaseVersion> {
    const KEY: &str = "<key>CFBundleShortVersionString</key>";
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!--") {
        plain.push_str(&rest[..at]);
        let end = rest[at..].find("-->")?;
        rest = &rest[at + end + "-->".len()..];
    }
    plain.push_str(rest);
    let at = plain.find(KEY)? + KEY.len();
    let value = plain[at..].trim_start().strip_prefix("<string>")?;
    ReleaseVersion::parse(value[..value.find("</string>")?].trim())
}

/// The image a keeper serves as ([`ExeImage`]): run from its own copy, the origin `exe` with the
/// stamp the process that made the copy took of it before copying ([`ORIGIN_STAMP_ENV`]), so
/// that an update landing during the copy reads as a replacement; run from `exe` itself, that
/// file now. `None` with no `exe`, or no stamp to read.
#[must_use]
pub fn started_image(from_copy: bool, exe: Option<&Path>) -> Option<ExeImage> {
    let exe = exe?;
    if from_copy {
        let stamp = std::env::var(ORIGIN_STAMP_ENV).ok()?;
        return Some(ExeImage::new(exe.to_path_buf(), FileStamp::parse(&stamp)?));
    }
    ExeImage::at(exe).ok()
}

/// Marker file written into the copy's directory ([`mark_ran`], [`sweep`]).
pub const RAN: &str = aterm_uds::deadline::RAN;

/// Say, in `copy` (the directory [`own_copy`] names), that this keeper runs from it.
pub fn mark_ran(copy: &Path) {
    if let Err(e) = aterm_uds::deadline::try_mark_ran(copy) {
        eprintln!(
            "aterm keeper: cannot mark {} as run from: {e}",
            copy.display()
        );
    }
}

/// Where keepers keep the copies they run from.
#[must_use]
pub fn code_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    home.is_absolute().then(|| code_dir_in(&home))
}

/// [`code_dir`] for the absolute `home`.
#[must_use]
pub fn code_dir_in(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("aterm")
        .join("keeper-code")
}

fn own_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()?.canonicalize().ok()
}

/// The copy in `dir` this process runs from (`<dir>/<pid>-<start>`), if it runs from one.
#[must_use]
pub fn own_copy(dir: &Path) -> Option<PathBuf> {
    let dir = dir.canonicalize().ok()?;
    let exe = own_exe()?;
    let name = exe.strip_prefix(&dir).ok()?.components().next()?;
    Some(dir.join(name))
}

/// The executable this keeper was started as: what [`ORIGIN_ENV`] names when it runs from its copy.
#[must_use]
pub fn origin_exe(from_copy: bool) -> Option<PathBuf> {
    let named = std::env::var_os(ORIGIN_ENV)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    match named {
        Some(origin) if from_copy => Some(origin),
        _ => own_exe(),
    }
}

/// Copy `exe`'s code into `dir` and re-execute from it with [`ORIGIN_ENV`] naming `exe` and
/// [`ORIGIN_STAMP_ENV`] the file `image` found there before the copy.
#[must_use]
pub fn run_from_own_copy(
    exe: &Path,
    dir: &Path,
    identity: &Identity,
    image: Option<&ExeImage>,
) -> String {
    let copy = match copy_own_code(exe, dir, identity) {
        Ok(copy) => copy,
        Err(why) => return why,
    };
    aterm_uds::deadline::stamp_exec(&copy.home);
    let failed = reexec(&copy.exe, exe, image.map(ExeImage::stamp));
    let _ = std::fs::remove_dir_all(&copy.home);
    format!("re-running from {}: {failed}", copy.exe.display())
}

struct Copy {
    /// Its directory, `<dir>/<pid>-<seconds>-<micros>`.
    home: PathBuf,
    /// The executable in it to run.
    exe: PathBuf,
}

fn copy_name(pid: u32, birth: Birth) -> String {
    format!("{pid}-{}-{}", birth.seconds, birth.micros)
}

/// The keeper (its pid and start) whose copy, or half-made `.partial` copy, `name` names: none
/// for any other name.
#[must_use]
pub fn owner_of(name: &str) -> Option<(u32, Birth)> {
    let name = name.strip_suffix(".partial").unwrap_or(name);
    let mut parts = name.split('-');
    let pid: u32 = parts
        .next()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()?;
    let seconds = parts.next()?.parse().ok()?;
    let micros = parts.next()?.parse().ok()?;
    let birth = Birth { seconds, micros };
    (parts.next().is_none() && copy_name(pid, birth) == name).then_some((pid, birth))
}

fn copy_own_code(exe: &Path, dir: &Path, identity: &Identity) -> Result<Copy, String> {
    let at = |e: io::Error| format!("{}: {e}", dir.display());
    if let Some(parent) = dir.parent() {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(at)?;
    }
    crate::server::ensure_private_dir(dir).map_err(at)?;
    let pid = std::process::id();
    let birth = crate::server::born(pid).ok_or("this process's start time cannot be read")?;
    let name = copy_name(pid, birth);
    let source = crate::job::own_bundle(exe).unwrap_or_else(|| exe.to_path_buf());
    let inner = source
        .parent()
        .and_then(|p| exe.strip_prefix(p).ok())
        .ok_or_else(|| format!("{} is not inside {}", exe.display(), source.display()))?
        .to_path_buf();
    let top = source.file_name().ok_or("the code has no name")?;
    let home = dir.join(&name);
    let partial = dir.join(format!("{name}.partial"));
    let made = std::fs::create_dir(&partial)
        .and_then(|()| copy_tree(&source, &partial.join(top)))
        .and_then(|()| std::fs::rename(&partial, &home));
    if let Err(e) = made {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!(
            "copying {} into {}: {e}",
            source.display(),
            dir.display()
        ));
    }
    if let Err(why) = identity.admits_code_at(&home.join(top)) {
        let _ = std::fs::remove_dir_all(&home);
        return Err(format!(
            "its copy was not admitted by this build's check: {why}"
        ));
    }
    Ok(Copy {
        exe: home.join(inner),
        home,
    })
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    copy_tree_kind(src, dst, std::fs::symlink_metadata(src)?.file_type())
}

fn copy_tree_kind(src: &Path, dst: &Path, kind: std::fs::FileType) -> io::Result<()> {
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if kind.is_dir() {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let meta = std::fs::symlink_metadata(src)?;
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            copy_tree_kind(&entry.path(), &dst.join(entry.file_name()), ft)?;
        }
        let dir_fd = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(dst)?;
        let perms = meta.permissions();
        if dir_fd
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o7777 == perms.mode() & 0o7777)
        {
            return Ok(());
        }
        dir_fd.set_permissions(perms)
    } else if kind.is_file() {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let (mut source, src_meta) = aterm_uds::regular_file::open(src)?;
        let perms = src_meta.permissions();
        // A clone (APFS) shares the source's blocks: it writes no byte and takes no free space,
        // so the copy never waits on the disk's write throughput or room (a debug build's
        // executable is 600 MB, an 8 KiB read/write loop through `std::io::copy`). A file
        // system that cannot clone gets the byte copy.
        let sink = if aterm_uds::regular_file::clone_held(&source, dst)? {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
                .open(dst)?
        } else {
            let mut sink = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(perms.mode())
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
                .open(dst)?;
            std::io::copy(&mut source, &mut sink)?;
            sink
        };
        if sink
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o7777 == perms.mode() & 0o7777)
        {
            return Ok(());
        }
        sink.set_permissions(perms)
    } else {
        Err(io::Error::other(format!(
            "{} is not a file, a directory or a link",
            src.display()
        )))
    }
}

/// Become `copy`, with [`ORIGIN_ENV`] naming `origin` and [`ORIGIN_STAMP_ENV`] its `stamp`
/// (none removes one this process inherited, which names another file).
fn reexec(copy: &Path, origin: &Path, stamp: Option<FileStamp>) -> io::Error {
    let mut args = std::env::args_os();
    let mut cmd = aterm_uds::spawnfd::command(copy);
    if let Some(arg0) = args.next() {
        cmd.arg0(arg0);
    }
    cmd.args(args).env(ORIGIN_ENV, origin);
    match stamp {
        Some(stamp) => cmd.env(ORIGIN_STAMP_ENV, stamp.encode()),
        None => cmd.env_remove(ORIGIN_STAMP_ENV),
    };
    cmd.exec()
}

/// Remove every copy in `dir` whose keeper is gone.
pub fn sweep(dir: &Path, born: impl Fn(u32) -> Option<Birth>) -> usize {
    sweep_at(dir, born, std::time::SystemTime::now())
}

/// [`sweep`] at `now`.
fn sweep_at(dir: &Path, born: impl Fn(u32) -> Option<Birth>, now: std::time::SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            if !owner_of(name).is_some_and(|(pid, birth)| born(pid) != Some(birth)) {
                return false;
            }
            if !may_go(&entry.path(), name.ends_with(".partial"), now) {
                eprintln!(
                    "aterm keeper: kept {}: its keeper ended before it said it ran from it, so \
                     macOS may still be assessing its first exec",
                    entry.path().display()
                );
                return false;
            }
            std::fs::remove_dir_all(entry.path()).is_ok()
        })
        .count()
}

/// Whether `home` may go ([`RAN`], [`run_from_own_copy`], [`aterm_uds::deadline::copy_may_go`]).
fn may_go(home: &Path, partial: bool, now: std::time::SystemTime) -> bool {
    partial || aterm_uds::deadline::copy_may_go(home, now)
}

#[cfg(test)]
#[path = "code_tests.rs"]
mod tests;
