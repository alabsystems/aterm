// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Overlay homes ([`Seam::Overlay`]): a pooled CLI with no login-only variable runs each login in
//! a home of its own, whose shared entries (sessions, history, the owner's setup, the databases)
//! are links to the default home, so a session one login wrote resumes under another. [`ensure`]
//! makes the links and the overlay's own markers that are missing, and reports every entry that
//! is something else ([`Drift`]); it never moves or replaces an entry of either home, and removes
//! only a [`Shared::Present`] link to the default home's entry while that entry is missing.

use std::io;
use std::path::{Component, Path, PathBuf};

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
/// A shared name is not a relative path within its home, a nested overlay parent is not a real
/// private directory, `overlay` or a shared directory cannot be made, the default home's root cannot be listed for
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
    for name in shared
        .iter()
        .filter_map(|entry| match entry {
            Shared::Dir(name) | Shared::File(name) | Shared::Present(name) => Some(*name),
            Shared::Suffix(_) => None,
        })
        .chain(markers.iter().copied())
    {
        if Path::new(name).components().next().is_none()
            || !Path::new(name)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "shared entries must be relative paths within their home",
            ));
        }
    }
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
    let text = super::accounts::read_bounded(&target, 1024 * 1024)?.unwrap_or_default();
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
        super::atomic_write::write_atomic_mode(&target, &edited, None)?;
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
    private_parents(overlay, name)?;
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
    private_parents(overlay, name)?;
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
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
    private_parents(overlay, name)?;
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

/// Nested entries keep real private ancestors in the overlay. A foreign directory link
/// cannot redirect creation of a shared leaf outside the claimed home.
fn private_parents(overlay: &Path, name: &str) -> io::Result<()> {
    let mut at = overlay.to_path_buf();
    if let Some(parent) = Path::new(name).parent() {
        for component in parent.components() {
            at.push(component.as_os_str());
            match std::fs::symlink_metadata(&at) {
                Ok(meta) if !meta.file_type().is_dir() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "shared entry parent is not a real directory",
                    ));
                }
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
            aterm_types::fs_restricted::ensure_private_dir(&at)?;
        }
    }
    Ok(())
}

/// Why an overlay home cannot be made where aterm makes no symbolic links ([`make_link`]).
pub(crate) const NO_LINKS: &str =
    "an overlay home needs symbolic links, which aterm makes only on Unix";

/// Why `row`'s login home of `cli` cannot be made here, judged from the records alone: a row of
/// its own (a non-empty `dir`) whose seam is an overlay home ([`Seam::Overlay`], Kimi Code's)
/// where aterm makes no symbolic links ([`NO_LINKS`]). `None` on Unix and for every other row.
/// A launch meets the same refusal when it prepares such a row ([`make_link`]), so a judge that
/// says what a launch takes without preparing one (`accounts_pool::launch_now`) meets it too.
pub(crate) fn unmade_here(
    cli: super::accounts::Cli,
    row: &super::accounts::Row,
) -> Option<&'static str> {
    let own = row.dir.as_deref().is_some_and(|dir| !dir.is_empty());
    (cfg!(not(unix)) && own && matches!(cli.accounts().seam, Seam::Overlay { .. }))
        .then_some(NO_LINKS)
}

/// Whether aterm makes `cli`'s login homes here: every CLI's on Unix, and off Unix each but an
/// overlay home's ([`unmade_here`]), whose refusal off Unix
/// `a_sign_in_that_needs_an_overlay_runs_nothing_where_aterm_makes_no_links` and
/// `an_overlay_row_is_never_started_on_where_aterm_makes_no_links` hold.
#[cfg(test)]
pub(crate) fn links_here(cli: super::accounts::Cli) -> bool {
    cfg!(unix) || !matches!(cli.accounts().seam, Seam::Overlay { .. })
}

/// The pooled CLI a harness model's `provider` names (1 Codex, 2 Grok Build, 3 Kimi Code CLI), as
/// the launch policy, pool recovery and sign-in policy walks read it beside [`links_here`].
#[cfg(test)]
pub(crate) fn model_provider(index: i64) -> super::accounts::Cli {
    use super::accounts::Cli;
    match index {
        1 => Cli::Codex,
        2 => Cli::Grok,
        3 => Cli::Kimi,
        _ => unreachable!("a model's provider is 1..=3: {index}"),
    }
}

#[cfg(unix)]
fn make_link(target: &Path, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(not(unix))]
fn make_link(_target: &Path, _at: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, NO_LINKS))
}

#[cfg(all(test, unix))]
#[path = "accounts_overlay_tests.rs"]
mod tests;
