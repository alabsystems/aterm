// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit developer setup for git's existing vault credential route. This never copies an
//! app image, invokes the credential protocol, changes a vault entry, or changes another
//! helper's configuration. The authenticated configuration policy is read without exposing
//! token values.
//! Run it from a protected development executable; the ordinary installed app remains alone.

use crate::{Error, SecretBytes, fsx};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

const LIMIT: usize = 1024 * 1024;
const KEYS: [&str; 2] = [
    "credential.https://github.com.helper",
    "credential.https://gist.github.com.helper",
];

/// Public, value-free result of an explicit local setup.
#[derive(Debug)]
pub struct Report {
    /// Actual protected executable used by the credential helper.
    pub program: PathBuf,
    /// Content identity of the protected developer image, independent of its build cache.
    pub program_sha256: String,
    /// Configuration files with recognized helper replacements.
    pub files: Vec<PathBuf>,
    /// Private exact rollback copies, retained only on an applied change.
    pub backup: Option<PathBuf>,
}

struct Change {
    path: PathBuf,
    original: SecretBytes,
    edited: SecretBytes,
    identity: (u64, u64),
}

fn error(why: impl Into<String>) -> Error {
    Error::new(why)
}

fn word(path: &Path) -> crate::Result<&str> {
    let text = path
        .to_str()
        .ok_or_else(|| error("helper path is not UTF-8"))?;
    if !path.is_absolute()
        || path
            .components()
            .any(|p| !matches!(p, Component::RootDir | Component::Normal(_)))
        || !text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.@+".contains(&b))
    {
        return Err(error(
            "helper needs an absolute path without shell syntax, whitespace or parent components",
        ));
    }
    Ok(text)
}

/// Refuse writeable ancestry instead of hiding it behind a symlink or copying it elsewhere.
/// The Darwin development route checks ACLs with the system's read-only `ls` too. Other
/// systems retain the existing helper and report that this setup is unavailable.
fn protected_path(program: &Path) -> crate::Result<()> {
    word(program)?;
    #[cfg(not(target_os = "macos"))]
    {
        Err(error(
            "protected developer helper setup is currently Darwin-only",
        ))
    }
    #[cfg(target_os = "macos")]
    {
        for path in program.ancestors() {
            let meta = std::fs::symlink_metadata(path)?;
            if meta.file_type().is_symlink()
                || (!meta.is_file() && !meta.is_dir() && path == program)
                || (!meta.is_dir() && path != program)
                || (meta.uid() != 0 && meta.uid() != fsx::euid())
                || meta.mode() & 0o022 != 0
            {
                return Err(error(format!(
                    "{}: helper path is linked, foreign, or writable by another account",
                    path.display()
                )));
            }
            let out = aterm_uds::spawnfd::command("/bin/ls")
                .args(["-lde", "--"])
                .arg(path)
                .env("LC_ALL", "C")
                .output()?;
            if !out.status.success() {
                return Err(error("cannot read the helper path's access lists"));
            }
            let text = std::str::from_utf8(&out.stdout)
                .map_err(|_| error("helper path access-list report is not UTF-8"))?;
            if text.lines().skip(1).any(|line| line.contains(" allow ")) {
                return Err(error(format!(
                    "{}: helper path has an ACL grant",
                    path.display()
                )));
            }
        }
        Ok(())
    }
}

fn protected(program: &Path) -> crate::Result<()> {
    protected_path(program)?;
    let meta = std::fs::metadata(program)?;
    if !meta.is_file() || meta.uid() != fsx::euid() || meta.nlink() != 1 || meta.mode() & 0o111 == 0
    {
        return Err(error(
            "developer helper must be this user's single-link executable",
        ));
    }
    Ok(())
}

struct Image {
    path: PathBuf,
    digest: String,
    bytes: Vec<u8>,
}

impl Image {
    fn prepare(home: &Path, program: &Path) -> crate::Result<Self> {
        protected(program)?;
        let bytes = fsx::read_owned(program, 512 * 1024 * 1024)?
            .ok_or_else(|| error("protected developer image disappeared"))?;
        protected(program)?;
        let digest = crate::materialize::digest_hex(&bytes);
        let path = home
            .join(".local/libexec/aterm/git-helper")
            .join(&digest)
            .join("aterm");
        word(&path)?;
        Ok(Self {
            path,
            digest,
            bytes,
        })
    }

    fn install(&self) -> crate::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| error("helper image has no parent"))?;
        // Only create missing directories. Existing directories are judged, never chmodded.
        let mut directories: Vec<_> = parent.ancestors().collect();
        directories.reverse();
        for dir in directories {
            match std::fs::symlink_metadata(dir) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::DirBuilder::new().mode(0o700).create(dir)?;
                }
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }
            protected_path(dir)?;
            if !std::fs::symlink_metadata(dir)?.is_dir() {
                return Err(error("helper image parent is not a real directory"));
            }
        }
        match fsx::read_owned(&self.path, 512 * 1024 * 1024)? {
            Some(held) if held != self.bytes => {
                return Err(error(
                    "existing content-addressed helper differs; preserved",
                ));
            }
            Some(_) => {}
            None => fsx::write_atomic(&self.path, &self.bytes, 0o700)?,
        }
        protected(&self.path)?;
        Ok(())
    }
}

/// The old vault's helper grammar, not an arbitrary shell-command heuristic. The returned
/// dev login is transported to the existing native helper; all other byte sequences refuse.
fn legacy(value: &str) -> Option<&str> {
    const PREFIX: &str = r#"!f() { [ "$1" = get ] || exit 0; u=; while IFS='=' read -r k v; do [ "$k" = username ] && u="$v"; done; case "$u" in alabsystems) t=gh_access_token_alabsystems ;; ''|"#;
    const SUFFIX: &str = r#"; t=gh_access_token ;; *) exit 0 ;; esac; p=$(cat "$HOME/.secrets/$t" 2>/dev/null) && [ -n "$p" ] || { echo quit=1; exit 0; }; printf 'username=%s\npassword=%s\n' "$u" "$p"; }; f"#;
    let middle = value.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let (login, repeated) = middle.split_once(") u=")?;
    (login == repeated && crate::cli::is_github_login(login)).then_some(login)
}

fn replacement(value: &str, home: &Path, program: &Path) -> crate::Result<Option<String>> {
    if value.is_empty() {
        return Ok(None);
    }
    if let Some(login) = legacy(value) {
        return Ok(Some(format!(
            "!{} secrets git-credential --dev-user {login}",
            word(program)?
        )));
    }
    let old = format!(
        "!{} secrets git-credential",
        home.join(".local/bin/aterm").display()
    );
    let current = format!("!{} secrets git-credential", word(program)?);
    for route in [old, current.clone()] {
        if value == route {
            return Ok((value != current).then_some(current));
        }
        if let Some(login) = value.strip_prefix(&format!("{route} --dev-user "))
            && crate::cli::is_github_login(login)
        {
            let new = format!("{current} --dev-user {login}");
            return Ok((value != new).then_some(new));
        }
    }
    Err(error(
        "custom credential helper left unchanged; only the exact legacy vault or native aterm route can be migrated",
    ))
}

fn git(file: &Path, args: &[&str]) -> crate::Result<std::process::Output> {
    let out = aterm_uds::spawnfd::command("/usr/bin/git")
        .args(["config", "--file"])
        .arg(file)
        .args(args)
        .output()?;
    if out.status.success() || out.status.code() == Some(1) && out.stdout.is_empty() {
        Ok(out)
    } else {
        Err(error(
            "git could not read or stage credential settings (output withheld)",
        ))
    }
}

fn plan(home: &Path, program: &Path) -> crate::Result<Vec<Change>> {
    let mut changes = Vec::new();
    for rel in [".config/git/config", ".gitconfig"] {
        let path = home.join(rel);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(_) => protected_path(&path)?,
        }
        let Some(original) = fsx::read_owned(&path, LIMIT)? else {
            continue;
        };
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.nlink() != 1 {
            return Err(error("configuration has more than one link"));
        }
        let scratch = aterm_tempfile::TempDir::new()?;
        let stage = scratch.path().join("config");
        fsx::write_atomic(&stage, original.as_slice(), 0o600)?;
        let mut changed = false;
        for key in KEYS {
            let values = git(&stage, &["--null", "--get-all", key])?;
            let text = std::str::from_utf8(&values.stdout)
                .map_err(|_| error("credential helper is not UTF-8"))?;
            for value in text.split_terminator('\0') {
                if let Some(new) = replacement(value, home, program)? {
                    git(
                        &stage,
                        &["--fixed-value", "--replace-all", key, &new, value],
                    )?;
                    changed = true;
                }
            }
        }
        if changed {
            let edited = fsx::read_private(&stage, LIMIT)?
                .ok_or_else(|| error("staged configuration disappeared"))?;
            changes.push(Change {
                path,
                original: original.into(),
                edited,
                identity: (meta.dev(), meta.ino()),
            });
        }
    }
    Ok(changes)
}

struct Locked {
    path: PathBuf,
    file: File,
}

impl Drop for Locked {
    fn drop(&mut self) {
        if let (Ok(named), Ok(opened)) =
            (std::fs::symlink_metadata(&self.path), self.file.metadata())
            && (named.dev(), named.ino()) == (opened.dev(), opened.ino())
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn unchanged(change: &Change) -> crate::Result<()> {
    let meta = std::fs::symlink_metadata(&change.path)?;
    let now =
        fsx::read_owned(&change.path, LIMIT)?.ok_or_else(|| error("configuration disappeared"))?;
    if (meta.dev(), meta.ino()) != change.identity || now.as_slice() != change.original.as_slice() {
        return Err(error(
            "configuration changed while setup prepared it; no stale replacement made",
        ));
    }
    Ok(())
}

fn apply(home: &Path, program: &Path, changes: &[Change]) -> crate::Result<Option<PathBuf>> {
    if changes.is_empty() {
        return Ok(None);
    }
    protected(program)?;
    let mut locks = Vec::new();
    for change in changes {
        let mut name = change.path.as_os_str().to_os_string();
        name.push(".lock");
        let path = PathBuf::from(name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        locks.push(Locked { path, file });
    }
    for change in changes {
        unchanged(change)?;
    }
    // The rollback data is private and value-bearing; only its directory is reported.
    let backup = aterm_tempfile::Builder::new()
        .prefix("aterm-git-helper-")
        .tempdir_in(home)?
        .keep();
    for (index, (change, lock)) in changes.iter().zip(&mut locks).enumerate() {
        fsx::write_atomic(
            &backup.join(format!("{index}.original")),
            change.original.as_slice(),
            0o600,
        )?;
        fsx::write_atomic(
            &backup.join(format!("{index}.path")),
            change.path.as_os_str().as_encoded_bytes(),
            0o600,
        )?;
        lock.file.write_all(change.edited.as_slice())?;
        aterm_durable::durable(&lock.file)?;
    }
    for (change, lock) in changes.iter().zip(&locks) {
        unchanged(change)?;
        std::fs::rename(&lock.path, &change.path)?;
    }
    Ok(Some(backup))
}

/// Prepare, and only when requested apply, an exact known-helper migration. Git's normal
/// `.lock` files serialize its writers; complete input bytes and inode identity are checked
/// after all locks and immediately before each rename. Unknown helpers or concurrent edits
/// refuse. A partial filesystem failure leaves private rollback copies, never an automatic
/// overwrite of a peer's later change. This is local setup, not a publication attestation.
///
/// # Errors
/// The executable is not protected, a file/helper is unsupported, git cannot stage its config,
/// a peer holds a lock or changes input, or an I/O operation fails.
pub fn setup(home: &Path, program: &Path, write: bool) -> crate::Result<Report> {
    let image = Image::prepare(home, program)?;
    let changes = plan(home, &image.path)?;
    let paths = crate::client::Paths::locate_for_home(home);
    let tracked = crate::client::delivered_files(&paths)?;
    for change in &changes {
        if tracked.contains(&change.path) {
            let expected = crate::client::config(&paths)?
                .ok_or_else(|| error("tracked Git configuration has no vault source"))?;
            let rel = change
                .path
                .strip_prefix(home)
                .map_err(|_| error("configuration leaves the actual home"))?;
            let name = format!("~/{}", rel.display());
            let entry = crate::client::read_verified_entry(&paths, &expected, &name)?
                .ok_or_else(|| error("Git configuration is not an intact authenticated default"))?;
            if !entry.unanchored_default() || entry.value().as_slice() != change.original.as_slice()
            {
                return Err(error(
                    "always-managed or anchored Git configuration requires its owner's change; local setup did nothing",
                ));
            }
        }
    }
    let backup = if write {
        image.install()?;
        apply(home, &image.path, &changes)?
    } else {
        None
    };
    Ok(Report {
        program: image.path,
        program_sha256: image.digest,
        files: changes.into_iter().map(|c| c.path).collect(),
        backup,
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
