// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private bundle whose executable bytes come from the admitted open file.

use std::path::{Path, PathBuf};

use super::Executable;

#[cfg(target_os = "macos")]
#[path = "artifact_snapshot_images.rs"]
mod images;
#[cfg(target_os = "macos")]
pub(crate) use images::Images;

#[derive(Debug)]
pub(crate) struct Snapshot {
    directory: Option<aterm_tempfile::TempDir>,
    app: PathBuf,
    facts: [u8; aterm_stamp::facts::LEN],
    attempted: bool,
}

impl Snapshot {
    #[cfg(unix)]
    pub(crate) fn prepare(app: &Path, executable: &Executable) -> Result<Self, String> {
        let directory = aterm_tempfile::Builder::new()
            .prefix("aterm-admitted-")
            .suffix(".noindex")
            // A caller's TMPDIR may itself be a temporary guard that removes all children.
            // The system temporary root survives that caller and keeps uncertain execs intact.
            .tempdir_in("/tmp")
            .map_err(|e| e.to_string())?;
        Self::prepare_in(app, executable, directory)
    }

    #[cfg(unix)]
    fn prepare_in(
        app: &Path,
        executable: &Executable,
        directory: aterm_tempfile::TempDir,
    ) -> Result<Self, String> {
        use std::os::unix::fs::symlink;

        let mut snapshot = Self {
            app: directory.path().join("aterm.app"),
            directory: Some(directory),
            facts: executable.facts,
            attempted: false,
        };
        let root = open_directory(app)?;
        copy_directory(&root, Path::new(""), &snapshot.app)?;
        let bin = snapshot.app.join("Contents/MacOS");
        // Directory modes are sealed only after these owned aliases and the held image exist.
        executable.copy_held(&bin.join("aterm"))?;
        for alias in ["aterm-gui", "aterm-ctl"] {
            symlink("aterm", bin.join(alias)).map_err(|e| e.to_string())?;
        }
        snapshot.app = std::fs::canonicalize(&snapshot.app).map_err(|e| e.to_string())?;
        check_links(&snapshot.app, &snapshot.app)?;
        seal_directories(&snapshot.app)?;
        Ok(snapshot)
    }

    #[cfg(not(unix))]
    pub(crate) fn prepare(_app: &Path, _executable: &Executable) -> Result<Self, String> {
        Err("private bundle measurement is only supported on Unix".into())
    }

    pub(crate) fn app(&self) -> &Path {
        &self.app
    }

    /// Each executable spelling is a first execution of its own path. Prove all three before
    /// handing any path to a delayed consumer; the reply precedes application startup.
    pub(crate) fn prove(&mut self, ctx: &crate::Ctx) -> Result<(), String> {
        for name in ["aterm", "aterm-gui", "aterm-ctl"] {
            let mut cmd = crate::exec::Cmd::new(self.app.join("Contents/MacOS").join(name))
                .arg(aterm_stamp::facts::ARG)
                .env("PATH", &ctx.path_env);
            cmd.cwd = Some(ctx.root.clone());
            cmd.hang_after = Some(std::time::Duration::from_secs(60));
            self.attempted = true;
            let run = crate::exec::run(&cmd, ctx.exec_env());
            if !run.ok || run.output.as_bytes() != self.facts.as_slice() {
                return Err(format!(
                    "private {name} did not prove its admitted compile facts \
                     (status {:?}, spawn error {:?}, reply {} bytes, detail {:?}); retained {}",
                    run.code,
                    run.spawn_error,
                    run.output.len(),
                    run.output.chars().take(1024).collect::<String>(),
                    self.app.display()
                ));
            }
            aterm_stamp::facts::decode(run.output.as_bytes())?;
        }
        Ok(())
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if self.attempted {
            // Neither a successful probe nor an ordinary callback return proves that every
            // later descendant retired. Keep the namespace when no such custody receipt exists.
            if let Some(directory) = self.directory.take() {
                let _ = directory.keep();
            }
        }
    }
}

#[cfg(unix)]
fn open_directory(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("open bundle directory {}: {e}", path.display()))
}

#[cfg(unix)]
fn image_entry(relative: &Path) -> bool {
    ["aterm", "aterm-gui", "aterm-ctl"]
        .iter()
        .any(|name| relative == Path::new("Contents/MacOS").join(name))
}

#[cfg(unix)]
fn copy_directory(
    source: &std::fs::File,
    relative: &Path,
    destination: &Path,
) -> Result<(), String> {
    use aterm_dirfd::{AtFlags, FileType, Mode, OFlags};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    std::fs::create_dir(destination).map_err(|e| e.to_string())?;
    let entries = aterm_dirfd::Dir::read_from(source).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let name_path = std::ffi::OsStr::from_bytes(name.to_bytes());
        let relative = relative.join(name_path);
        if image_entry(&relative) {
            continue;
        }
        let to = destination.join(name_path);
        let metadata = aterm_dirfd::fstatat(source, name_path, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| e.to_string())?;
        let kind = FileType::from_raw_mode(metadata.st_mode);
        if (relative == Path::new("Contents") || relative == Path::new("Contents/MacOS"))
            && !kind.is_dir()
        {
            return Err(
                "the private executable's parent components must be real directories".into(),
            );
        }
        if kind.is_dir() {
            let descriptor = aterm_dirfd::openat(
                source,
                name_path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| e.to_string())?;
            let directory = std::fs::File::from(descriptor);
            copy_directory(&directory, &relative, &to)?;
        } else if kind.is_symlink() {
            let target = read_link(source, name)?;
            if target.is_absolute() || !contained_link(&relative, &target) {
                return Err(format!(
                    "bundle link {} leaves its private snapshot",
                    relative.display()
                ));
            }
            symlink(target, to).map_err(|e| e.to_string())?;
        } else if kind.is_file() {
            let descriptor = aterm_dirfd::openat(
                source,
                name_path,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(|e| e.to_string())?;
            let mut reader = std::fs::File::from(descriptor);
            if !reader.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("bundle resource changed its file type".into());
            }
            let mut writer = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&to)
                .map_err(|e| e.to_string())?;
            std::io::copy(&mut reader, &mut writer).map_err(|e| e.to_string())?;
            writer
                .set_permissions(std::fs::Permissions::from_mode(metadata.st_mode & 0o555))
                .map_err(|e| e.to_string())?;
        } else {
            return Err(format!(
                "bundle entry {} is not a directory, file or private link",
                relative.display()
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_link(source: &std::fs::File, name: &std::ffi::CStr) -> Result<PathBuf, String> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStringExt as _;

    let mut bytes = vec![0; 65536];
    // SAFETY: source is open, name is NUL-terminated, and the buffer is writable for its length.
    let count = unsafe {
        libc::readlinkat(
            source.as_raw_fd(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    let count = usize::try_from(count).map_err(|_| std::io::Error::last_os_error().to_string())?;
    if count == bytes.len() {
        return Err("bundle link exceeds its bounded spelling".into());
    }
    bytes.truncate(count);
    Ok(std::ffi::OsString::from_vec(bytes).into())
}

#[cfg(unix)]
fn contained_link(relative: &Path, target: &Path) -> bool {
    let mut depth = relative
        .parent()
        .map_or(0, |path| path.components().count());
    for component in target.components() {
        match component {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if depth > 0 => depth -= 1,
            _ => return false,
        }
    }
    true
}

#[cfg(unix)]
fn check_links(root: &Path, path: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            let target = std::fs::canonicalize(entry.path()).map_err(|e| e.to_string())?;
            if !target.starts_with(root) {
                return Err("private bundle link resolves outside its snapshot".into());
            }
        } else if kind.is_dir() {
            check_links(root, &entry.path())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn seal_directories(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            seal_directories(&entry.path())?;
        }
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500))
        .map_err(|e| e.to_string())
}

#[cfg(all(test, unix))]
#[path = "artifact_snapshot_tests.rs"]
mod tests;
