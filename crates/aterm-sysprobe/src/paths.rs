// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Path containment rules across Windows path spellings.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// The comparison key of an absolute Windows path, or `None` where it cannot be placed.
#[must_use]
pub fn windows_key(p: &Path) -> Option<String> {
    key_of(p.to_str()?)
}

/// Whether `path` is `dir` or lies under it, by [`windows_key`], without a mount table.
#[must_use]
pub fn windows_under(path: &Path, dir: &Path) -> Option<bool> {
    let (path, dir) = (windows_key(path)?, windows_key(dir)?);
    if key_under(&path, &dir) {
        return Some(true);
    }
    (root(&path) == root(&dir)).then_some(false)
}

fn root(key: &str) -> &str {
    let nth = if key.starts_with(r"\\") { 3 } else { 0 };
    let end = key
        .match_indices('\\')
        .nth(nth)
        .map_or(key.len(), |(i, _)| i + 1);
    &key[..end]
}

/// Whether key `key` is `dir` or lies under it, component by component (`C:\AB` is not under `C:\A`).
#[inline]
#[must_use]
pub fn key_under(key: &str, dir: &str) -> bool {
    key.strip_prefix(dir)
        .is_some_and(|rest| rest.is_empty() || dir.ends_with('\\') || rest.starts_with('\\'))
}

/// Whether `path` is `dir` or lies under it on the current platform without a mount table.
#[must_use]
pub fn is_under(path: &Path, dir: &Path) -> Option<bool> {
    if cfg!(windows) {
        windows_under(path, dir)
    } else {
        Some(path.starts_with(dir))
    }
}

/// Strips the Windows verbatim prefix (`\\?\`) from `p` for display.
#[must_use]
pub fn plain(p: &Path) -> Cow<'_, Path> {
    let Some(rest) = p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) else {
        return Cow::Borrowed(p);
    };
    if rest.contains('/') {
        return Cow::Borrowed(p);
    }
    if let Some(share) = rest
        .get(..4)
        .filter(|u| u.eq_ignore_ascii_case(r"UNC\"))
        .and_then(|_| rest.get(4..))
    {
        return if !share.starts_with('\\') && unc_root(share).is_some() {
            Cow::Owned(PathBuf::from(format!(r"\\{share}")))
        } else {
            Cow::Borrowed(p)
        };
    }
    match rest.as_bytes() {
        [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic() => Cow::Owned(PathBuf::from(rest)),
        _ => Cow::Borrowed(p),
    }
}

/// Split representation of an absolute Windows path containing 8.3 short-name components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortSpelled<'a> {
    /// Path prefix before the first 8.3 short name.
    pub lead: &'a Path,
    /// Verbatim Win32 path through the last 8.3 short name.
    pub through: PathBuf,
    /// Path components after the last 8.3 short name.
    pub rest: Vec<&'a str>,
}

/// Splits `p` into [`ShortSpelled`] when 8.3 short names are the only reason [`windows_key`] refuses it.
#[must_use]
pub fn short_spelled(p: &Path) -> Option<ShortSpelled<'_>> {
    let s = p.to_str()?;
    if key_of(s).is_some() || key_with(s, true).is_none() {
        return None;
    }
    let (_, rest, verbatim) = root_of(s)?;
    let root = &s[..s.len() - rest.len()];
    let mut lead = None;
    let mut at = root.len();
    for name in rest.split(['\\', '/']) {
        if short_shaped(name) {
            lead = Some(Path::new(&s[..at]));
            break;
        }
        at += name.len() + 1;
    }
    let names: Vec<&str> = rest.split(['\\', '/']).filter(|n| !n.is_empty()).collect();
    let last = names.iter().rposition(|n| short_shaped(n))?;
    let mut through = verbatim_root(root, verbatim);
    through.push_str(&names[..=last].join(r"\"));
    Some(ShortSpelled {
        lead: lead?,
        through: PathBuf::from(through),
        rest: names[last + 1..].to_vec(),
    })
}

fn verbatim_root(root: &str, verbatim: bool) -> String {
    if verbatim {
        let v = &root[4..];
        if v.get(..4).is_some_and(|u| u.eq_ignore_ascii_case(r"UNC\")) {
            return format!(r"\\?\UNC\{}", &v[4..]);
        }
        return format!(r"\\?\{v}");
    }
    match root.as_bytes() {
        [a, b, ..] if (*a == b'\\' || *a == b'/') && (*b == b'\\' || *b == b'/') => {
            let share = root[2..].replace('/', r"\");
            format!(r"\\?\UNC\{}\", share.trim_end_matches('\\'))
        }
        _ => format!(r"\\?\{}\", &root[..2]),
    }
}

fn key_of(s: &str) -> Option<String> {
    key_with(s, false)
}

fn key_with(s: &str, short: bool) -> Option<String> {
    let (mut key, rest, verbatim) = root_of(s)?;
    if verbatim && s.contains('/') {
        return None;
    }
    let rest = rest.trim_end_matches(['\\', '/']);
    if rest.is_empty() {
        return Some(key);
    }
    for name in rest.split(['\\', '/']) {
        if name.is_empty() {
            if verbatim {
                return None;
            }
            continue;
        }
        if name == "."
            || name == ".."
            || (!verbatim && name.ends_with(['.', ' ']))
            || (!short && short_shaped(name))
        {
            return None;
        }
        if !key.ends_with('\\') {
            key.push('\\');
        }
        key.extend(name.chars().map(fold));
    }
    Some(key)
}

fn root_of(s: &str) -> Option<(String, &str, bool)> {
    if let Some(v) = s.strip_prefix(r"\\?\").or_else(|| s.strip_prefix(r"\??\")) {
        let (root, rest) = match v.get(..4) {
            Some(unc) if unc.eq_ignore_ascii_case(r"UNC\") => unc_root(&v[4..])?,
            _ => drive_root(v, true)?,
        };
        return Some((root, rest, true));
    }
    let sep = |b: u8| b == b'\\' || b == b'/';
    match s.as_bytes() {
        [a, b, c, d, ..] if sep(*a) && sep(*b) && (*c == b'.' || *c == b'?') && sep(*d) => None,
        [a, b, ..] if sep(*a) && sep(*b) => {
            let (root, rest) = unc_root(&s[2..])?;
            Some((root, rest, false))
        }
        _ => {
            let (root, rest) = drive_root(s, false)?;
            Some((root, rest, false))
        }
    }
}

fn drive_root(s: &str, verbatim: bool) -> Option<(String, &str)> {
    match s.as_bytes() {
        [drive, b':', sep, ..]
            if drive.is_ascii_alphabetic() && (*sep == b'\\' || (!verbatim && *sep == b'/')) =>
        {
            Some((
                format!("{}:\\", char::from(drive.to_ascii_uppercase())),
                &s[3..],
            ))
        }
        _ => None,
    }
}

fn unc_root(s: &str) -> Option<(String, &str)> {
    let mut it = s.splitn(3, ['\\', '/']);
    let (server, share) = (it.next()?, it.next()?);
    if server.is_empty() || share.is_empty() {
        return None;
    }
    let mut root = String::from(r"\\");
    root.extend(server.chars().map(fold));
    root.push('\\');
    root.extend(share.chars().map(fold));
    root.push('\\');
    Some((root, it.next().unwrap_or("")))
}

fn short_shaped(name: &str) -> bool {
    let (base, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let Some((_, digits)) = base.rsplit_once('~') else {
        return false;
    };
    !base.contains('.')
        && base.chars().count() <= 8
        && ext.chars().count() <= 3
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
}

const SIMPLE_ONLY: &[(u32, u32, u32)] = &[
    (0x1F80, 0x1F87, 8),
    (0x1F90, 0x1F97, 8),
    (0x1FA0, 0x1FA7, 8),
    (0x1FB3, 0x1FB3, 9),
    (0x1FC3, 0x1FC3, 9),
    (0x1FF3, 0x1FF3, 9),
];

/// Folds `c` to its single-unit simple uppercase as Windows case-insensitive comparison does.
#[must_use]
pub fn fold_case(c: char) -> char {
    fold(c)
}

fn fold(c: char) -> char {
    let u = u32::from(c);
    if u > 0xFFFF {
        return c;
    }
    if let Some((_, _, offset)) = SIMPLE_ONLY
        .iter()
        .find(|(first, last, _)| (*first..=*last).contains(&u))
    {
        return char::from_u32(u + offset).unwrap_or(c);
    }
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) if u32::from(u) <= 0xFFFF => u,
        _ => c,
    }
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
