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
/// [`InstallError::Refused`] on an invalid request, or [`InstallError::Failed`] on I/O failure.
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
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("not a regular file".to_string());
    }
    let too_large = |bytes: usize| ImportError::TooLarge { bytes }.to_string();
    if metadata.len() > MAX_IMPORT_BYTES as u64 {
        return Err(too_large(
            usize::try_from(metadata.len()).unwrap_or(usize::MAX),
        ));
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_IMPORT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(too_large(bytes.len()));
    }
    Ok(bytes)
}

/// Write `bytes` to `dest` through a temporary file in the same folder.
///
/// # Errors
/// [`InstallError::Exists`] if `dest` exists without `replace`, or [`InstallError::Failed`].
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
        .tempdir_in(dir)
        .map_err(|e| failed(dir, e))?;
    let tmp = stage.path().join("theme");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| failed(&tmp, e))?;
    let written = file
        .write_all(bytes)
        .and_then(|()| aterm_durable::durable(&file));
    drop(file);
    written.map_err(|e| failed(&tmp, e))?;
    if replace {
        again_while_refused(|| std::fs::rename(&tmp, dest)).map_err(|e| failed(dest, e))
    } else {
        // Only an exclusive operation can publish without replacement: a
        // theme may appear after any check that the name is free.
        publish_new(&tmp, dest, |from, to| std::fs::hard_link(from, to))
    }
}

/// How many times [`again_while_refused`] tries the replace again.
#[cfg(windows)]
const REFUSED_TRIES: u64 = 30;

/// `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION` (unmapped in [`std::io::ErrorKind`]).
#[cfg(windows)]
const HELD_OS_ERRORS: [i32; 2] = [32, 33];

/// `op`, tried again while Windows refuses it because another process holds one of its files.
fn again_while_refused<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    #[cfg(windows)]
    {
        let mut tries = 0;
        loop {
            match op() {
                Err(e)
                    if (e.kind() == std::io::ErrorKind::PermissionDenied
                        || e.raw_os_error()
                            .is_some_and(|c| HELD_OS_ERRORS.contains(&c)))
                        && tries < REFUSED_TRIES =>
                {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(tries));
                }
                done => return done,
            }
        }
    }
    #[cfg(not(windows))]
    {
        op()
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
mod tests {
    use super::*;

    const KITTY: &str = "## name: Harbour\nforeground #c0caf5\nbackground #1a1b26\n";

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-types-install-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// A refused replace is tried again on Windows until it goes through,
    /// and a refusal that never lifts is still the caller's error after a
    /// bounded number of tries; any other error is returned at once.
    #[test]
    fn a_refused_replace_is_tried_again_only_on_windows_and_only_so_often() {
        use std::io::{Error, ErrorKind};
        let mut calls = 0u64;
        let out = again_while_refused(|| {
            calls += 1;
            if calls < 3 {
                Err(Error::from(ErrorKind::PermissionDenied))
            } else {
                Ok(calls)
            }
        });
        if cfg!(windows) {
            assert_eq!(out.unwrap(), 3, "two refusals, then the replace");
        } else {
            assert_eq!(out.unwrap_err().kind(), ErrorKind::PermissionDenied);
            assert_eq!(calls, 1);
        }

        let mut calls = 0u64;
        let out: std::io::Result<()> = again_while_refused(|| {
            calls += 1;
            Err(Error::from(ErrorKind::PermissionDenied))
        });
        assert_eq!(out.unwrap_err().kind(), ErrorKind::PermissionDenied);
        #[cfg(windows)]
        assert_eq!(calls, REFUSED_TRIES + 1, "bounded");
        #[cfg(not(windows))]
        assert_eq!(calls, 1);

        for code in [32, 33] {
            let mut calls = 0u64;
            let out = again_while_refused(|| {
                calls += 1;
                if calls < 3 {
                    Err(Error::from_raw_os_error(code))
                } else {
                    Ok(calls)
                }
            });
            if cfg!(windows) {
                assert_eq!(
                    out.unwrap(),
                    3,
                    "os error {code}: two holds, then the replace"
                );
            } else {
                assert_eq!(out.unwrap_err().raw_os_error(), Some(code));
                assert_eq!(calls, 1);
            }
        }

        let others: [fn() -> Error; 2] = [
            || Error::from(ErrorKind::NotFound),
            || Error::from_raw_os_error(2),
        ];
        for error in others {
            let mut calls = 0u64;
            let out: std::io::Result<()> = again_while_refused(|| {
                calls += 1;
                Err(error())
            });
            assert_eq!(out.unwrap_err().kind(), ErrorKind::NotFound);
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn a_failed_exclusive_publication_never_falls_back_to_overwriting() {
        let root = scratch("exclusive");
        let tmp = root.join("pending");
        let dest = root.join("mine.conf");
        std::fs::write(&tmp, "new theme").unwrap();

        let failed = publish_new(&tmp, &dest, |_, dest| {
            std::fs::write(dest, "other writer")?;
            Err(std::io::ErrorKind::Unsupported.into())
        });
        assert!(failed.is_err());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "other writer");
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "new theme");

        std::fs::remove_file(&dest).unwrap();
        let failed = publish_new(&tmp, &dest, |_, _| {
            Err(std::io::ErrorKind::Unsupported.into())
        });
        assert!(failed.is_err(), "exclusive publication was unavailable");
        assert!(
            !dest.exists(),
            "a failed publication must not install a theme"
        );
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "new theme");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An arbitrary export filename can be our usual temporary basename.
    #[test]
    fn publication_keeps_a_destination_named_like_its_temporary_file() {
        const BARE_NAME: &str = "ATERM_TYPES_SCHEME_INSTALL_BARE_NAME";
        const LAST: &str = "foreground = #000000\nbackground = #ffffff\n";
        let id = format!("export-alias-{:?}", std::thread::current().id());
        let name = format!(".{id}.conf.import-{}", std::process::id());
        let publish = |parent: &Path, dest: &Path| {
            for (replace, text) in [
                (false, "foreground = #ffffff\nbackground = #000000\n"),
                (true, LAST),
            ] {
                write_atomically(parent, dest, &id, text.as_bytes(), replace).unwrap();
                let actual = std::fs::read_to_string(dest).expect("published destination remains");
                assert_eq!(actual, text);
                assert_eq!(
                    scheme::parse_theme_str(&actual).unwrap(),
                    scheme::parse_theme_str(text).unwrap()
                );
            }
        };
        if std::env::var_os(BARE_NAME).is_some() {
            let bare = PathBuf::from(&name);
            assert_eq!(
                std::fs::symlink_metadata(&bare).unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
            publish(Path::new("."), &bare);
            return;
        }
        let dir = scratch("temp-destination");
        publish(&dir, &dir.join(&name));
        let cwd = dir.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let test = format!(
            "{}::publication_keeps_a_destination_named_like_its_temporary_file",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
            .args(["--exact", &test, "--nocapture", "--test-threads=1"])
            .env(BARE_NAME, "1")
            .current_dir(&cwd)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .expect("re-exec the test binary");
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the re-exec failed:\n{said}");
        assert!(
            said.contains("1 passed"),
            "the re-exec ran no test:\n{said}"
        );
        let left: Vec<_> = std::fs::read_dir(&cwd)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(
            left[0]
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".export-alias-")),
            "{left:?}"
        );
        assert_eq!(std::fs::read_to_string(&left[0]).unwrap(), LAST);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A window's import keeps the theme's own name, steps around a
    /// built-in's id, and never replaces an existing theme.
    #[test]
    fn import_as_new_keeps_the_display_name_and_steps_around_builtins() {
        let root = scratch("as-new");
        let themes = root.join("themes");
        let source = root.join("Nord.conf");
        std::fs::write(&source, KITTY).unwrap();
        let installed = install_theme_file_as_new(&source, Some(&themes)).unwrap();
        assert_eq!(installed.id, "Nord (imported)");
        assert_eq!(installed.dest, themes.join("Nord (imported).conf"));
        let parsed =
            scheme::parse_theme_str(&std::fs::read_to_string(&installed.dest).unwrap()).unwrap();
        assert_eq!(parsed.scheme.name, "Harbour", "the source's own name stays");

        let again = install_theme_file_as_new(&source, Some(&themes));
        assert_eq!(again, Err(InstallError::Exists(installed.dest.clone())));
        let named = install_theme_file(&source, Some("mine"), false, Some(&themes)).unwrap();
        let parsed =
            scheme::parse_theme_str(&std::fs::read_to_string(&named.dest).unwrap()).unwrap();
        assert_eq!(parsed.scheme.name, "mine");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An import whose id is an existing theme's in another case writes
    /// nothing, even with `replace`: the pair would disable both themes.
    #[test]
    fn import_refuses_a_case_variant_of_an_existing_theme() {
        let root = scratch("case");
        let themes = root.join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        let existing = themes.join("Harbour.conf");
        std::fs::write(&existing, KITTY).unwrap();
        let source = root.join("harbour.conf");
        std::fs::write(&source, KITTY).unwrap();
        for replace in [false, true] {
            let out = install_theme_file(&source, None, replace, Some(&themes));
            assert!(
                matches!(&out, Err(InstallError::Failed(m)) if m.contains("only in case")),
                "replace={replace}: {out:?}"
            );
        }
        let names: Vec<_> = std::fs::read_dir(&themes)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["Harbour.conf"], "nothing was written");
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), KITTY);
        install_theme_file(&source, Some("Lighthouse"), false, Some(&themes)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_refused_request_reads_and_writes_nothing() {
        let root = scratch("refused");
        let themes = root.join("themes");
        for (path, name) in [
            ("https://example.com/x.conf", None),
            ("/nonexistent/x.conf", Some("Default")),
            ("/nonexistent/x.conf", Some("dark:x")),
        ] {
            let out = install_theme_file(Path::new(path), name, false, Some(&themes));
            assert!(
                matches!(out, Err(InstallError::Refused(_))),
                "{path} {name:?}: {out:?}"
            );
        }
        assert!(!themes.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Publishing must not remove a preexisting staging-name collision;
    /// a failed commit must clean only the stage this call created.
    #[test]
    fn atomic_writes_keep_foreign_stage_entries_and_clean_owned_failures() {
        let owned = aterm_tempfile::TempDir::new().expect("owned fixture");
        let dir = owned.path().join("themes");
        std::fs::create_dir(&dir).unwrap();
        let id = "owned-stage";
        let collision = dir.join(format!(".{id}.conf.import-{}", std::process::id()));
        std::fs::write(&collision, b"keep collision").unwrap();
        let dest = dir.join("ordinary.conf");
        write_atomically(&dir, &dest, id, b"first", false).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"first");
        assert!(matches!(
            write_atomically(&dir, &dest, id, b"refused", false),
            Err(InstallError::Exists(_))
        ));
        assert_eq!(std::fs::read(&dest).unwrap(), b"first");
        write_atomically(&dir, &dest, id, b"replacement", true).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"replacement");

        let directory = dir.join("cannot-replace-directory");
        std::fs::create_dir(&directory).unwrap();
        assert!(write_atomically(&dir, &directory, "failure", b"not published", true).is_err());
        assert!(directory.is_dir());
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        let mut wanted = vec![
            collision.file_name().unwrap().to_os_string(),
            dest.file_name().unwrap().to_os_string(),
            directory.file_name().unwrap().to_os_string(),
        ];
        wanted.sort();
        assert_eq!(
            names, wanted,
            "no owned stage remains and the collision stays"
        );
        assert_eq!(std::fs::read(&collision).unwrap(), b"keep collision");
    }
}
