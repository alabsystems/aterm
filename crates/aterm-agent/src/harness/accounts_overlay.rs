// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Overlay homes ([`Seam::Overlay`]): a pooled CLI with no login-only variable runs each login in
//! a home of its own, whose shared entries (sessions, history, the owner's setup, the databases)
//! are links to the default home, so a session one login wrote resumes under another. [`ensure`]
//! makes the links that are missing and reports every entry that is something else ([`Drift`]);
//! it never moves, replaces or removes an entry of either home.

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
    /// The shared entries it left as they were, since they are not its link.
    pub drift: Vec<Drift>,
}

/// Makes `overlay` (mode `0700` where it is new) a home for `seam`'s CLI whose shared entries
/// link to `default_home` ([`Shared`]): a [`Shared::Dir`] missing from the default home is made
/// there first, so its link never dangles; a [`Shared::File`] is linked whether or not it exists
/// yet; a [`Shared::Suffix`] links each file at the default home's root whose name ends so. An
/// entry that is already its link is left; any other entry of that name is reported as
/// [`Drift`] and left. A seam that is no overlay does nothing.
/// # Errors
///
/// `overlay` or a shared directory cannot be made, the default home's root cannot be listed for
/// a [`Shared::Suffix`], an entry's kind cannot be read, or a link cannot be made.
pub fn ensure(seam: Seam, overlay: &Path, default_home: &Path) -> io::Result<Ensured> {
    let Seam::Overlay { shared, .. } = seam else {
        return Ok(Ensured::default());
    };
    aterm_types::fs_restricted::ensure_private_dir(overlay)?;
    let mut out = Ensured::default();
    for entry in shared {
        match *entry {
            Shared::Dir(name) => {
                std::fs::create_dir_all(default_home.join(name))?;
                link(overlay, default_home, name, &mut out)?;
            }
            Shared::File(name) => link(overlay, default_home, name, &mut out)?,
            Shared::Suffix(suffix) => {
                let mut names = suffixed(default_home, suffix)?;
                names.sort();
                for name in names {
                    link(overlay, default_home, &name, &mut out)?;
                }
            }
        }
    }
    Ok(out)
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
