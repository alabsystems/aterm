// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keep a macOS remote child on the code its caller loaded, across a nested rebuild.

#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
struct PublishedImage {
    owner: u32,
    path: PathBuf,
}

#[cfg(target_os = "macos")]
static IMAGE: std::sync::OnceLock<PublishedImage> = std::sync::OnceLock::new();

/// An executable copy owned by this CLI, removed when its ordinary dispatch returns.
/// A fork cannot remove its parent's image. Other hosts need no copy: Linux's remote
/// boundary already executes `/proc/self/exe`, and Windows retains its existing remote behavior.
#[must_use]
pub struct HostImage {
    #[cfg(target_os = "macos")]
    dir: Option<aterm_tempfile::TempDir>,
    #[cfg(target_os = "macos")]
    owner: u32,
    #[cfg(target_os = "macos")]
    name: std::ffi::OsString,
}

/// Capture the transport host before dispatch can rebuild its launch path.
/// Keep the returned owner alive until all ordinary CLI work has returned.
///
/// # Errors
/// The loaded image is unavailable, changed before/during capture, cannot be copied,
/// or this host has already been captured. No installed or PATH transport is substituted.
pub fn pin() -> Result<HostImage, String> {
    #[cfg(target_os = "macos")]
    {
        let source = std::env::current_exe().map_err(|e| e.to_string())?;
        let host = capture(&source, || {})?;
        IMAGE
            .set(PublishedImage {
                owner: std::process::id(),
                path: host.path(),
            })
            .map_err(|_| "remote host was already captured".to_owned())?;
        Ok(host)
    }
    #[cfg(not(target_os = "macos"))]
    Ok(HostImage {})
}

#[cfg(target_os = "macos")]
pub(crate) fn image() -> Result<&'static Path, String> {
    IMAGE
        .get()
        .filter(|host| host.owner == std::process::id())
        .map(|host| host.path.as_path())
        .ok_or_else(|| "remote host was not captured before dispatch".to_owned())
}

/// Remove this process's captured host immediately before an in-place exec.
/// The new image must capture its own host; a failed exec cannot reuse the removed one.
///
/// # Errors
/// A foreign process inherited the image name, or its owned directory cannot be removed.
pub fn release_before_exec() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if let Some(host) = IMAGE.get() {
        if host.owner != std::process::id() {
            return Err("a foreign process cannot release the remote host".to_owned());
        }
        let directory = host.path.parent().expect("private host directory");
        std::fs::remove_dir_all(directory).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
impl HostImage {
    fn path(&self) -> PathBuf {
        self.dir
            .as_ref()
            .expect("owned image")
            .path()
            .join(&self.name)
    }
}

#[cfg(target_os = "macos")]
impl Drop for HostImage {
    fn drop(&mut self) {
        if self.owner != std::process::id() {
            // A fork inherited the guard, not ownership of its parent's directory.
            std::mem::forget(self.dir.take());
        }
    }
}

#[cfg(target_os = "macos")]
fn stamp(m: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt as _;
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

#[cfg(target_os = "macos")]
fn capture(source: &Path, copied: impl FnOnce()) -> Result<HostImage, String> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let refused = || "remote host launch path no longer names the loaded image".to_owned();
    let pid = i32::try_from(std::process::id()).map_err(|_| refused())?;
    let loaded = aterm_uds::process::LoadedImage::read(pid).ok_or_else(refused)?;
    // Agh chooses its callback interface after canonicalizing the host image path.
    let resolved = source.canonicalize().map_err(|e| e.to_string())?;
    let still_loaded = || {
        loaded.matches_file(source) == Some(true)
            && loaded.matches_file(&resolved) == Some(true)
            && source.canonicalize().is_ok_and(|now| now == resolved)
    };
    let mut input = std::fs::File::open(&resolved).map_err(|e| e.to_string())?;
    let before = input.metadata().map_err(|e| e.to_string())?;
    if !loaded.matches_metadata(&before) || !still_loaded() {
        return Err(refused());
    }
    let name = resolved.file_name().ok_or_else(refused)?.to_owned();
    let dir = aterm_tempfile::Builder::new()
        .prefix("verify-remote-host-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.path().join(&name))
        .map_err(|e| e.to_string())?;
    let count = std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
    copied();
    let after = input.metadata().map_err(|e| e.to_string())?;
    if count != before.len() || stamp(&before) != stamp(&after) || !still_loaded() {
        return Err(refused());
    }
    output
        .set_permissions(std::fs::Permissions::from_mode(0o500))
        .map_err(|e| e.to_string())?;
    drop(output);
    Ok(HostImage {
        dir: Some(dir),
        owner: std::process::id(),
        name,
    })
}

#[cfg(all(test, target_os = "macos"))]
#[path = "remote_host_tests.rs"]
mod tests;
