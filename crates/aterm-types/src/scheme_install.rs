// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Installing an imported theme into the themes folder: the one write path
//! behind `aterm theme import` and Settings' "Import…".
//!
//! [`crate::scheme_import`] converts text and touches no file. This module is
//! the file half around it: it reads one local file (never a URL) up to the
//! theme-file bound, picks the theme's id (the caller's name, else the file's
//! stem; never a built-in's name), checks that the native file it is about to
//! write reads back to the converted theme, and writes it atomically into the
//! themes folder. Without `replace` an existing theme of that id is left
//! alone. Both front ends call [`install_theme_file`], so the command line and
//! the window cannot disagree about what an import writes.

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
    /// The request itself is wrong (a URL, a bad or built-in id): nothing was
    /// read.
    Refused(String),
    /// A theme with this id is already in the themes folder, and the
    /// import was not asked to replace it.
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

/// Convert the theme at `path` and write it into `theme_dir` as
/// `<id>.conf`, where `id` is `name` or else the file's stem. `theme_dir` is
/// `None` when no themes folder could be found.
///
/// # Errors
///
/// [`InstallError::Refused`] before anything is read when `path` is a URL or
/// the id is unusable; [`InstallError::Failed`] when the file cannot be read
/// or converted, the result does not read back unchanged, or the write fails
/// (including an existing theme of that id without `replace`).
pub fn install_theme_file(
    path: &Path,
    name: Option<&str>,
    replace: bool,
    theme_dir: Option<&Path>,
) -> Result<InstalledTheme, InstallError> {
    install_with(path, name, name, replace, theme_dir)
}

/// Import the theme at `path` as a NEW theme, the way a window's "Import…"
/// does: the id is the file's stem, or the stem with ` (imported)` after it
/// when the stem is a built-in's name (a kitty `Nord.conf`, say), and the
/// theme keeps its own display name. An existing theme of that id is never
/// replaced.
///
/// # Errors
///
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

/// The import: `id` is the theme's id (else the file's stem); `rename` is
/// the scheme a multi-scheme source is picked by and the display name the
/// theme takes (else its own).
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

    // The id: `name`, else the file's stem.
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
    // The written file must read back as exactly what was converted: the
    // native parser is the only reader the window has.
    let expected = ParsedTheme {
        scheme: report.scheme.clone(),
        description: report.description.clone(),
        author: report.author.clone(),
        appearance_declared: report.appearance_declared,
    };
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
    // Checked even with `replace`: replacing `<id>.conf` would still leave
    // the other-case file beside it, and the catalog loads neither.
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

/// A themes-folder file whose name is `<id>.conf` in another ASCII case. The
/// catalog refuses every file of such a pair (it cannot tell which one
/// `theme = "<id>"` means), so a writer that adds the second file breaks the
/// first. `None` when there is none, or the folder cannot be read.
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

/// Read a regular file, refusing one larger than the theme bound before
/// reading it (and reading at most one byte past the bound, should it grow).
///
/// # Errors
///
/// The reason, as text, when the file is missing, not regular, unreadable or
/// over the bound.
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

/// Write `bytes` to `dest` through a temporary file in the same folder, so a
/// reader (the window's catalog) sees the old file or the whole new one,
/// never a part. Without `replace` an existing `dest` is left alone: the new
/// file is linked in under that name only if the name is free. Every theme
/// file aterm writes (`import`, `new`, `export`, Settings' "Import…") goes
/// through here.
///
/// # Errors
///
/// [`InstallError::Exists`] when `dest` is taken and `replace` is false;
/// [`InstallError::Failed`] when the write fails.
pub fn write_atomically(
    dir: &Path,
    dest: &Path,
    id: &str,
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
    // Not `.conf`, so the catalog never lists a half-written file.
    let mut tmp = dir.join(format!(".{id}.conf.import-{}", std::process::id()));
    // An export can choose any filename, including our usual temporary name.
    // Keep its destination distinct even when its parent is spelled `.`.
    if tmp.file_name() == dest.file_name() {
        tmp.set_extension(format!("import-{}.tmp", std::process::id()));
    }
    let _ = std::fs::remove_file(&tmp);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(failed(&tmp, e));
    }
    let committed = if replace {
        std::fs::rename(&tmp, dest).map_err(|e| failed(dest, e))
    } else {
        // Only an exclusive operation can publish without replacement: a
        // theme may appear after any check that the name is free.
        publish_new(&tmp, dest, |from, to| std::fs::hard_link(from, to))
    };
    let _ = std::fs::remove_file(&tmp);
    committed
}

/// Link `tmp` in as `dest` only if `dest` is free. A filesystem that cannot
/// link fails closed: falling back to a rename could clobber a theme another
/// writer created after the name was checked.
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

    #[test]
    fn a_failed_exclusive_publication_never_falls_back_to_overwriting() {
        let root = scratch("exclusive");
        let tmp = root.join("pending");
        let dest = root.join("mine.conf");
        std::fs::write(&tmp, "new theme").unwrap();

        let failed = publish_new(&tmp, &dest, |_, dest| {
            // A second writer may take the destination while publication is
            // attempted. Its contents belong to that writer.
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
    /// Publication and cleanup must leave the complete destination behind,
    /// including when the destination is spelled without its `.` parent.
    #[test]
    fn publication_keeps_a_destination_named_like_its_temporary_file() {
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let dir = scratch("temp-destination");
        let id = format!("export-alias-{:?}", std::thread::current().id());
        let name = format!(".{id}.conf.import-{}", std::process::id());
        let relative = PathBuf::from(&name);
        assert_eq!(
            std::fs::symlink_metadata(&relative).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        let _relative_cleanup = Cleanup(relative.clone());
        let explicit = dir.join(&name);
        let _explicit_cleanup = Cleanup(explicit.clone());
        for (parent, dest) in [
            (Path::new("."), relative.as_path()),
            (dir.as_path(), &explicit),
        ] {
            for (replace, text) in [
                (false, "foreground = #ffffff\nbackground = #000000\n"),
                (true, "foreground = #000000\nbackground = #ffffff\n"),
            ] {
                write_atomically(parent, dest, &id, text.as_bytes(), replace).unwrap();
                let actual = std::fs::read_to_string(dest).expect("published destination remains");
                assert_eq!(actual, text);
                assert_eq!(
                    scheme::parse_theme_str(&actual).unwrap(),
                    scheme::parse_theme_str(text).unwrap()
                );
            }
        }
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
        // Negative control: the command line's explicit name does rename.
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
        // Negative control: a name that is no case variant installs.
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
}
