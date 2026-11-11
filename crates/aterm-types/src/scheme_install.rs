// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Atomic theme installer ([`install_theme_file`], [`crate::scheme_import`]).

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::scheme::{self, ParsedTheme};
use crate::scheme_import::{self, ImportError, ImportFormat, MAX_IMPORT_BYTES};

/// What an import wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledTheme {
    /// The theme's id: its file stem, what `theme = "<id>"` names.
    pub id: String,
    /// The file written.
    pub dest: PathBuf,
    /// The format the source was read as.
    pub format: ImportFormat,
    /// The source's keys aterm has no place for.
    pub dropped: Vec<String>,
    /// What the conversion assumed or filled in.
    pub notes: Vec<String>,
}

/// Why an import wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// The request itself is wrong (a URL, a bad or built-in id): nothing was read.
    Refused(String),
    /// A theme with this id is already in the themes folder.
    Exists(PathBuf),
    /// The file could not be read or converted, or the write failed.
    Failed(String),
}

impl core::fmt::Display for InstallError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(message) | Self::Failed(message) => f.write_str(message),
            Self::Exists(dest) => write!(f, "{} already exists", dest.display()),
        }
    }
}

impl std::error::Error for InstallError {}

/// Convert the theme at `path` and write it into `theme_dir` as `<id>.conf`.
///
/// # Errors
/// [`InstallError::Refused`] before anything is read when `path` is a URL or the id is unusable;
/// [`InstallError::Exists`] for an existing theme of that id without `replace`;
/// [`InstallError::Failed`] when the file cannot be read or converted, the result does not read
/// back unchanged, `theme_dir` is `None` or holds `<id>.conf` in another case, or the write fails.
pub fn install_theme_file(
    path: &Path,
    name: Option<&str>,
    replace: bool,
    theme_dir: Option<&Path>,
) -> Result<InstalledTheme, InstallError> {
    install_with(path, name, name, replace, theme_dir)
}

/// Import the theme at `path` as a NEW theme, the way a window's "Import…" does.
///
/// # Errors
/// As [`install_theme_file`].
pub fn install_theme_file_as_new(
    path: &Path,
    theme_dir: Option<&Path>,
) -> Result<InstalledTheme, InstallError> {
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let shadowed = stem.is_some_and(|stem| scheme::builtin_theme(stem).is_some());
    let id = stem.map(|stem| {
        if shadowed {
            format!("{stem} (imported)")
        } else {
            stem.to_string()
        }
    });
    install_with(path, id.as_deref(), None, false, theme_dir)
}

/// The import: `id` is the theme's id (else the file's stem).
fn install_with(
    path: &Path,
    id: Option<&str>,
    rename: Option<&str>,
    replace: bool,
    theme_dir: Option<&Path>,
) -> Result<InstalledTheme, InstallError> {
    let shown = path.to_string_lossy().into_owned();
    if shown.contains("://") {
        return Err(InstallError::Refused(format!(
            "{shown:?} is a URL; import reads only a local file (download it first)"
        )));
    }

    let id = match id {
        Some(id) => id.to_string(),
        None => match path.file_stem().map(|stem| stem.to_str()) {
            Some(Some(stem)) => stem.to_string(),
            _ => {
                return Err(InstallError::Refused(
                    "cannot take a theme id from that file name; pass --name <id>".to_string(),
                ));
            }
        },
    };
    if let Err(why) = scheme::validate_user_theme_name(&id) {
        return Err(InstallError::Refused(format!(
            "{id:?} cannot be a theme id: {why}; pass --name <id>"
        )));
    }
    if let Some(builtin) = scheme::builtin_names()
        .into_iter()
        .find(|builtin| builtin.eq_ignore_ascii_case(&id))
    {
        return Err(InstallError::Refused(format!(
            "{id:?} is the built-in theme {builtin:?}; pass --name with another id"
        )));
    }

    let bytes = read_bounded(path)
        .map_err(|message| InstallError::Failed(format!("{shown}: {message}")))?;
    let file_name = path.file_name().and_then(|name| name.to_str());
    let mut report = scheme_import::import_theme(file_name, &bytes, rename)
        .map_err(|error| InstallError::Failed(format!("{shown}: {error}")))?;
    if rename.is_some() || report.scheme.name.is_empty() {
        report.scheme.name = scheme_import::sanitize_text(&id).unwrap_or_default();
    }

    let header_source = format!(
        "Imported by `aterm theme import` from {} ({}).",
        path.file_name()
            .map_or_else(|| shown.clone(), |n| n.to_string_lossy().into_owned()),
        report.format.label()
    );
    let text = report.to_native_conf(&[&header_source]);
    let expected = ParsedTheme::colours_only(
        report.scheme.clone(),
        report.appearance_declared,
        report.description.clone(),
        report.author.clone(),
    );
    match scheme::parse_theme_str(&text) {
        Ok(read_back) if read_back == expected => {}
        Ok(_) => {
            return Err(InstallError::Failed(
                "the converted theme does not read back unchanged; nothing was written".to_string(),
            ));
        }
        Err(error) => {
            return Err(InstallError::Failed(format!(
                "the converted theme does not read back ({error}); nothing was written"
            )));
        }
    }

    let Some(dir) = theme_dir else {
        return Err(InstallError::Failed(
            "no themes folder: neither XDG_CONFIG_HOME, APPDATA nor HOME is set".to_string(),
        ));
    };
    let dest = dir.join(format!("{id}.conf"));
    if let Some(other) = case_variant(dir, &id) {
        return Err(InstallError::Failed(case_collision_message(&other, &id)));
    }
    write_atomically(dir, &dest, &id, text.as_bytes(), replace)?;
    Ok(InstalledTheme {
        id,
        dest,
        format: report.format,
        dropped: report.dropped,
        notes: report.notes,
    })
}

/// A themes-folder file whose name is `<id>.conf` in another ASCII case.
#[must_use]
pub fn case_variant(dir: &Path, id: &str) -> Option<PathBuf> {
    let wanted = format!("{id}.conf");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name != wanted && name.eq_ignore_ascii_case(&wanted))
        })
        .map(|entry| entry.path())
}

/// Why a write of `<id>.conf` was refused when [`case_variant`] found `other`.
#[must_use]
pub fn case_collision_message(other: &Path, id: &str) -> String {
    format!(
        "{} differs from {id:?} only in case, and the themes folder would load neither; \
         rename or remove it first",
        other.display()
    )
}

/// Read a regular file, refusing one larger than the theme bound before reading it.
///
/// # Errors
/// The reason, as text, when the file is missing, not regular, unreadable or over the bound.
pub fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let too_large = |bytes: usize| ImportError::TooLarge { bytes }.to_string();
    let (file, open_meta) =
        crate::fs_restricted::open_regular_nonblock_with_meta(path).map_err(|e| e.to_string())?;
    if open_meta.len() > MAX_IMPORT_BYTES as u64 {
        return Err(too_large(
            usize::try_from(open_meta.len()).unwrap_or(usize::MAX),
        ));
    }
    let cap = usize::try_from(open_meta.len())
        .unwrap_or(0)
        .min(MAX_IMPORT_BYTES)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(cap);
    file.take(MAX_IMPORT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(too_large(bytes.len()));
    }
    Ok(bytes)
}

/// Write `bytes` to `dest` through a temporary file in the same folder, durably flushed, then
/// flush the folder's entry best-effort ([`aterm_durable::Flush::dir_best_effort`]).
///
/// # Errors
/// [`InstallError::Exists`] if `dest` exists without `replace`, or [`InstallError::Failed`]: a
/// failed write leaves `dest` as it was, and a failed flush of the folder leaves the theme in place.
pub fn write_atomically(
    dir: &Path,
    dest: &Path,
    _id: &str,
    bytes: &[u8],
    replace: bool,
) -> Result<(), InstallError> {
    use std::io::Write;
    let exists = |dest: &Path| std::fs::symlink_metadata(dest).is_ok();
    let taken = || InstallError::Exists(dest.to_path_buf());
    if !replace && exists(dest) {
        return Err(taken());
    }
    let failed =
        |path: &Path, e: std::io::Error| InstallError::Failed(format!("{}: {e}", path.display()));
    std::fs::create_dir_all(dir).map_err(|e| failed(dir, e))?;
    let stage = aterm_tempfile::Builder::new()
        .prefix(".theme-import-")
        .suffix(".noindex")
        .tempdir_in(dir)
        .map_err(|e| failed(dir, e))?;
    let tmp = stage.path().join("theme");
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        const FLAGS: i32 = 0x0100 | 0x0100_0000;
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        const FLAGS: i32 = 0o2_000_000;
        open.mode(0o644).custom_flags(FLAGS);
    }
    let mut file = open.open(&tmp).map_err(|e| failed(&tmp, e))?;
    let written = file
        .write_all(bytes)
        .and_then(|()| aterm_durable::durable(&file));
    drop(file);
    written.map_err(|e| failed(&tmp, e))?;
    if replace {
        again_while_refused(|| std::fs::rename(&tmp, dest)).map_err(|e| failed(dest, e))?;
    } else {
        // Only an exclusive operation can publish without replacement: a
        // theme may appear after any check that the name is free.
        publish_new(&tmp, dest, |from, to| std::fs::hard_link(from, to))?;
    }
    // The theme is in place; its folder's entry is flushed best-effort, a folder that cannot
    // be flushed here accepted and any other failure reported.
    let folder = match dest.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => folder,
        _ => Path::new("."),
    };
    let _flushed_or_unavailable = aterm_durable::Flush::Durable
        .dir_best_effort(folder)
        .map_err(|e| {
            InstallError::Failed(format!(
                "{} was written, but flushing {} failed, so it may not survive a power loss: {e}",
                dest.display(),
                folder.display()
            ))
        })?;
    Ok(())
}

/// A minute hang detector for a Windows file holder to release a required replace.
#[cfg(any(windows, test))]
const REPLACE_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION` (unmapped in [`std::io::ErrorKind`]).
#[cfg(any(windows, test))]
const HELD_OS_ERRORS: [i32; 2] = [32, 33];

/// `op`, tried again while Windows refuses it because another process holds one of its files.
fn again_while_refused<T>(op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    #[cfg(windows)]
    {
        again_while_refused_windows_at(op, std::time::Instant::now(), std::thread::sleep)
    }
    #[cfg(not(windows))]
    {
        let mut op = op;
        op()
    }
}

#[cfg(any(windows, test))]
fn again_while_refused_windows_at<T>(
    mut op: impl FnMut() -> std::io::Result<T>,
    started: std::time::Instant,
    mut pause: impl FnMut(std::time::Duration),
) -> std::io::Result<T> {
    let mut backoff = std::time::Duration::from_millis(1);
    loop {
        match op() {
            Err(e)
                if e.kind() == std::io::ErrorKind::PermissionDenied
                    || e.raw_os_error()
                        .is_some_and(|c| HELD_OS_ERRORS.contains(&c)) =>
            {
                let left = REPLACE_HANG.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Err(e);
                }
                pause(backoff.min(left));
                backoff = backoff
                    .saturating_add(std::time::Duration::from_millis(1))
                    .min(std::time::Duration::from_millis(30));
            }
            done => return done,
        }
    }
}

/// Link `tmp` in as `dest` only if `dest` is free.
fn publish_new(
    tmp: &Path,
    dest: &Path,
    hard_link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), InstallError> {
    match hard_link(tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(InstallError::Exists(dest.to_path_buf()))
        }
        Err(e) => Err(InstallError::Failed(format!(
            "{}: could not publish without replacement: {e}; use a filesystem that supports hard links, or pass --replace to permit overwriting",
            dest.display()
        ))),
    }
}

#[cfg(test)]
#[path = "scheme_install_tests.rs"]
mod tests;
