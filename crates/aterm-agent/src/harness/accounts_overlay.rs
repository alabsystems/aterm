// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Overlay homes ([`Seam::Overlay`]): a pooled CLI with no login-only variable runs each login in
//! a home of its own, whose shared entries (sessions, history, the owner's setup, the databases)
//! are links to the default home, so a session one login wrote resumes under another. [`ensure`]
//! makes the links and the overlay's own markers that are missing, and reports every entry that
//! is something else ([`Drift`]); it never moves or replaces an entry of either home, and removes
//! only a [`Shared::Present`] link to the default home's entry while that entry is missing.

use std::io;
use std::path::{Path, PathBuf};

use crate::ai::connector::{Seam, Shared};

/// A shared entry an overlay home holds as something other than the link [`ensure`] makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    /// The entry's name in the overlay home.
    pub name: String,
    /// What it is instead.
    pub is: Is,
}

/// What a drifted entry is ([`Drift`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Is {
    /// A real directory, which the CLI wrote apart from the default home.
    Dir,
    /// A real file (or another kind of entry), likewise.
    File,
    /// A link to somewhere other than the default home's entry.
    LinkTo(PathBuf),
}

/// What one [`ensure`] did and found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ensured {
    /// The links it made, by name, in the order the shared set names them.
    pub made: Vec<String>,
    /// The markers it wrote, by name ([`Seam::Overlay`]'s `markers`).
    pub marked: Vec<String>,
    /// The [`Shared::Present`] links it removed, by name, since the default home has no entry
    /// for them to lead to.
    pub unlinked: Vec<String>,
    /// The shared entries it left as they were, since they are not its link.
    pub drift: Vec<Drift>,
}

/// Makes `overlay` (mode `0700` where it is new) a home for `seam`'s CLI whose shared entries
/// link to `default_home` ([`Shared`]): a [`Shared::Dir`] missing from the default home is made
/// there first (mode `0700`, as kimi makes its own `sessions` and `locks`, the default home too
/// where it is missing), so its link never dangles; a [`Shared::File`] is linked whether or not
/// it exists yet; a [`Shared::Present`] is linked only once the default home has it, and until
/// then is nothing to check, except that an overlay's link to the default home's entry is
/// removed (one an earlier set made, or one whose file the person deleted), since kimi warns of
/// it at every start; a [`Shared::Suffix`] links each file at the default home's root whose name
/// ends so. An entry that is already its link is left; any other entry of that name is reported
/// as [`Drift`] and left. Each marker the overlay lacks is written there, empty (mode `0600`);
/// an entry of that name of any kind is left. A seam that is no overlay does nothing.
/// # Errors
///
/// `overlay` or a shared directory cannot be made, the default home's root cannot be listed for
/// a [`Shared::Suffix`], an entry's kind (a [`Shared::Present`]'s in the default home included)
/// or a link's target cannot be read, a link cannot be made or removed, or a marker cannot be
/// written.
pub fn ensure(seam: Seam, overlay: &Path, default_home: &Path) -> io::Result<Ensured> {
    let Seam::Overlay {
        shared, markers, ..
    } = seam
    else {
        return Ok(Ensured::default());
    };
    aterm_types::fs_restricted::ensure_private_dir(overlay)?;
    let mut out = Ensured::default();
    for entry in shared {
        match *entry {
            Shared::Dir(name) => {
                aterm_types::fs_restricted::create_dir_restricted(&default_home.join(name))?;
                link(overlay, default_home, name, &mut out)?;
            }
            Shared::File(name) => link(overlay, default_home, name, &mut out)?,
            Shared::Present(name) => match std::fs::symlink_metadata(default_home.join(name)) {
                Ok(_) => link(overlay, default_home, name, &mut out)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    unlink_own(overlay, default_home, name, &mut out)?;
                }
                Err(e) => return Err(e),
            },
            Shared::Suffix(suffix) => {
                let mut names = suffixed(default_home, suffix)?;
                names.sort();
                for name in names {
                    link(overlay, default_home, &name, &mut out)?;
                }
            }
        }
    }
    for &name in markers {
        mark(overlay, name, &mut out)?;
    }
    Ok(out)
}

/// Turn off Kimi Code's occasional session-rating prompt in `home`'s `tui.toml`, preserving
/// unrelated settings and comments. An invalid file is left untouched.
pub fn disable_feedback_survey(home: &Path) -> io::Result<()> {
    use aterm_toml::edit::{DocumentMut, Item, Value};

    let path = home.join("tui.toml");
    let target = match std::fs::read_link(&path) {
        Ok(target) if target.is_absolute() => target,
        Ok(target) => home.join(target),
        Err(_) => path,
    };
    let text = match std::fs::read_to_string(&target) {
        Ok(text) => text,
        Err(why) if why.kind() == io::ErrorKind::NotFound => String::new(),
        Err(why) => return Err(why),
    };
    let mut doc: DocumentMut = text.parse().map_err(|why| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Kimi Code's tui.toml is invalid and was left unchanged: {why}"),
        )
    })?;
    if doc["disable_feedback_survey"].as_bool() == Some(true) {
        return Ok(());
    }
    doc["disable_feedback_survey"] = Item::Value(Value::from(true));
    let edited = doc.to_string();
    let after: aterm_toml::Table = aterm_toml::from_str(&edited).map_err(|why| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("edited Kimi Code config would be invalid: {why}"),
        )
    })?;
    if after
        .get("disable_feedback_survey")
        .and_then(aterm_toml::Value::as_bool)
        != Some(true)
    {
        return Err(io::Error::other(
            "edited Kimi Code config did not retain the feedback-survey opt-out",
        ));
    }
    if text != edited {
        super::upgrade_models::write_atomic_mode(&target, &edited, None)?;
    }
    Ok(())
}

/// Removes `overlay`'s `name` where it is a link to `default_home`'s `name`; leaves any other
/// entry, and a link to anywhere else.
fn unlink_own(
    overlay: &Path,
    default_home: &Path,
    name: &str,
    out: &mut Ensured,
) -> io::Result<()> {
    let at = overlay.join(name);
    match std::fs::symlink_metadata(&at) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if std::fs::read_link(&at)? == default_home.join(name) {
                std::fs::remove_file(&at)?;
                out.unlinked.push(name.to_owned());
            }
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Writes `overlay`'s `name` empty (mode `0600`) where it has no entry of that name.
fn mark(overlay: &Path, name: &str, out: &mut Ensured) -> io::Result<()> {
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600);
    }
    match open.open(overlay.join(name)) {
        Ok(_) => {
            out.marked.push(name.to_owned());
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

fn suffixed(home: &Path, suffix: &str) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(home) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.ends_with(suffix) && entry.file_type()?.is_file() {
            names.push(name);
        }
    }
    Ok(names)
}

fn link(overlay: &Path, default_home: &Path, name: &str, out: &mut Ensured) -> io::Result<()> {
    let at = overlay.join(name);
    let target = default_home.join(name);
    match std::fs::symlink_metadata(&at) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            make_link(&target, &at)?;
            out.made.push(name.to_owned());
        }
        Err(e) => return Err(e),
        Ok(meta) if meta.file_type().is_symlink() => {
            let to = std::fs::read_link(&at)?;
            if to != target {
                out.drift.push(Drift {
                    name: name.to_owned(),
                    is: Is::LinkTo(to),
                });
            }
        }
        Ok(meta) => out.drift.push(Drift {
            name: name.to_owned(),
            is: if meta.is_dir() { Is::Dir } else { Is::File },
        }),
    }
    Ok(())
}

#[cfg(unix)]
fn make_link(target: &Path, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(not(unix))]
fn make_link(_target: &Path, _at: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "an overlay home needs symbolic links",
    ))
}

#[cfg(all(test, unix))]
#[path = "accounts_overlay_tests.rs"]
mod tests;
