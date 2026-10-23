// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH PATHS A WAKE MAY WATCH.

use std::path::{Path, PathBuf};

/// The longest path a wake may name, in bytes.
pub const PATH_MAX: usize = 1024;

/// The data volume's own mount: `/System/Volumes/Data/Users/<me>` is the home again.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The session's containment, as admission reads it: file atoms are the user containment's alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainmentWord {
    /// Home read-write.
    User,
    /// The project's folders.
    Safety,
    /// The OS sandbox.
    Containment,
}

/// What a probe of one path found, without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probed {
    /// Nothing is there.
    Missing,
    /// A folder.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link.
    Link,
    /// Anything else (a FIFO, a socket, a device), or a read refused.
    Other,
}

/// THE PROBE ADMISSION ASKS, injected: what is at a path, read as `lstat` reads it.
pub trait PathProbe {
    /// What is at `path`.
    fn probe(&self, path: &Path) -> Probed;
}

/// A path a wake may watch: absolute, and the root it is under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedPath {
    /// The path.
    pub path: PathBuf,
    /// The session root it is under.
    pub root: PathBuf,
}

/// Why a path is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRefusal {
    /// It lies under none of the session's roots.
    Outside,
    /// A component is `.`, `..` or dot-named.
    Dot,
    /// A component below its root names a credential.
    Credential,
    /// It lies under a privacy-guarded root.
    Guarded,
    /// A component is a symbolic link.
    Link,
    /// It is neither absolute nor under the home folder (`~/`).
    Relative,
    /// The session does not run under the user containment.
    Containment,
    /// It is longer than [`PATH_MAX`] bytes.
    Long,
    /// It holds a control character.
    Control,
    /// A component outside the home's own spelling holds a character outside ASCII.
    Unicode,
}

impl PathRefusal {
    /// Every refusal.
    pub const ALL: [PathRefusal; 10] = [
        PathRefusal::Outside,
        PathRefusal::Dot,
        PathRefusal::Credential,
        PathRefusal::Guarded,
        PathRefusal::Link,
        PathRefusal::Relative,
        PathRefusal::Containment,
        PathRefusal::Long,
        PathRefusal::Control,
        PathRefusal::Unicode,
    ];

    /// Its word (`ERR denied wake: path <word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            PathRefusal::Outside => "outside",
            PathRefusal::Dot => "dot",
            PathRefusal::Credential => "credential",
            PathRefusal::Guarded => "guarded",
            PathRefusal::Link => "link",
            PathRefusal::Relative => "relative",
            PathRefusal::Containment => "containment",
            PathRefusal::Long => "long",
            PathRefusal::Control => "control",
            PathRefusal::Unicode => "unicode",
        }
    }
}

/// Session roots: the home folder and the folders a wake is permitted to watch under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    /// The home folder (`~`).
    pub home: PathBuf,
    /// The roots, each absolute, below the home folder or beside it, none privacy-guarded.
    pub dirs: Vec<PathBuf>,
}

/// THE ROOTS OF A SESSION whose home is `home`.
#[must_use]
pub fn roots(home: &Path, cwds: &[PathBuf], scratch: &Path, protected: &[PathBuf]) -> Roots {
    let home_segs = absolute_segments(home);
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in std::iter::once(scratch).chain(cwds.iter().map(PathBuf::as_path)) {
        let Some(segs) = absolute_segments(dir) else {
            continue;
        };
        let above_home = home_segs
            .as_ref()
            .is_none_or(|home| fold(home).starts_with(&fold(&segs)));
        if above_home
            || segs.iter().any(|s| dotted(s))
            || !plain(&segs, home_segs.as_deref())
            || guarded(&segs, home_segs.as_deref(), protected)
        {
            continue;
        }
        let dir = joined(&segs);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    Roots {
        home: home.to_path_buf(),
        dirs,
    }
}

/// Admit a declared wake `path` (absolute, or `~/…` when `home_relative`) against `roots`.
/// # Errors
/// [`PathRefusal`]: the first reason, before any probe for every lexical one.
pub fn admit(
    path: &str,
    home_relative: bool,
    roots: &Roots,
    protected: &[PathBuf],
    mode: ContainmentWord,
    probe: &dyn PathProbe,
) -> Result<AdmittedPath, PathRefusal> {
    use super::fault::{Fault, armed};
    if armed(Fault::CanonicalizeAtRead) {
        let full = if home_relative {
            roots.home.join(path)
        } else {
            PathBuf::from(path)
        };
        let _ = probe.probe(&full);
    }
    if mode != ContainmentWord::User {
        return Err(PathRefusal::Containment);
    }
    if path.len() > PATH_MAX {
        return Err(PathRefusal::Long);
    }
    if path.chars().any(char::is_control) {
        return Err(PathRefusal::Control);
    }
    let home_segs = absolute_segments(&roots.home).ok_or(PathRefusal::Relative)?;
    let segs: Vec<String> = if home_relative {
        let mut segs = home_segs.clone();
        segs.extend(split(path));
        segs
    } else if path.starts_with('/') {
        split(path)
    } else {
        return Err(PathRefusal::Relative);
    };
    if split(path).iter().any(|s| dotted(s)) {
        return Err(PathRefusal::Dot);
    }
    if !plain(&segs, Some(&home_segs)) {
        return Err(PathRefusal::Unicode);
    }
    if guarded(&segs, Some(&home_segs), protected) {
        return Err(PathRefusal::Guarded);
    }
    let root = roots
        .dirs
        .iter()
        .filter_map(|dir| Some((dir, absolute_segments(dir)?)))
        .filter(|(_, root)| segs.starts_with(root))
        .max_by_key(|(_, root)| root.len())
        .ok_or(PathRefusal::Outside)?;
    if segs[root.1.len()..].iter().any(|s| credential(s)) {
        return Err(PathRefusal::Credential);
    }
    let first = if segs.starts_with(&home_segs) {
        home_segs.len()
    } else {
        1
    };
    for depth in first..=segs.len() {
        match probe.probe(&joined(&segs[..depth])) {
            Probed::Dir => {}
            Probed::Link => return Err(PathRefusal::Link),
            Probed::Missing | Probed::File | Probed::Other => break,
        }
    }
    Ok(AdmittedPath {
        path: joined(&segs),
        root: root.0.clone(),
    })
}

fn split(text: &str) -> Vec<String> {
    text.split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn absolute_segments(path: &Path) -> Option<Vec<String>> {
    let text = path.to_str()?;
    text.starts_with('/').then(|| split(text))
}

fn joined(segs: &[String]) -> PathBuf {
    PathBuf::from(format!("/{}", segs.join("/")))
}

fn fold(segs: &[String]) -> Vec<String> {
    segs.iter().map(|s| s.to_ascii_lowercase()).collect()
}

fn plain(segs: &[String], home: Option<&[String]>) -> bool {
    let below = match home {
        Some(home) if segs.starts_with(home) => &segs[home.len()..],
        _ => segs,
    };
    below.iter().all(|s| s.is_ascii())
}

fn dotted(seg: &str) -> bool {
    seg.starts_with('.')
}

fn credential(seg: &str) -> bool {
    let seg = seg.to_ascii_lowercase();
    ["credential", "secret", "token"]
        .iter()
        .any(|word| seg.contains(word))
        || seg.ends_with(".pem")
        || seg.ends_with(".key")
        || seg.starts_with("id_")
}

fn guarded(segs: &[String], home: Option<&[String]>, protected: &[PathBuf]) -> bool {
    let path = fold(segs);
    if path.starts_with(&fold(&split(DATA_VOLUME))) {
        return true;
    }
    let home = home.map(fold);
    protected.iter().any(|root| {
        let Some(root) = absolute_segments(root).map(|r| fold(&r)) else {
            return false;
        };
        if !path.starts_with(&root) {
            return false;
        }
        match &home {
            Some(home) if home.len() > root.len() && home.starts_with(&root) => {
                !path.starts_with(home)
            }
            _ => true,
        }
    })
}

#[cfg(test)]
#[path = "wake_atoms_tests.rs"]
mod tests;
