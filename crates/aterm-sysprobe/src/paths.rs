// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PATHS — the one rule that places a path under a directory across the spellings Windows hands out for one location.

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

/// A [`windows_key`]'s root (`C:\`, `\\SRV\SHARE\`): through its first `\`,
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

/// Whether `path` is `dir` or lies under it, by this platform's rule without a mount table: [`windows_under`] on Windows, `Path::starts_with` (always placed) elsewhere.
#[must_use]
pub fn is_under(path: &Path, dir: &Path) -> Option<bool> {
    if cfg!(windows) {
        windows_under(path, dir)
    } else {
        Some(path.starts_with(dir))
    }
}

/// `p` without the verbatim prefix `std::fs::canonicalize` answers with on Windows, for DISPLAY: `\\?\C:\x` reads `C:\x` and `\\?\UNC\srv\share\x` reads `\\srv\share\x`.
#[must_use]
pub fn plain(p: &Path) -> Cow<'_, Path> {
    let Some(rest) = p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) else {
        return Cow::Borrowed(p);
    };
    if rest.contains('/') {
        return Cow::Borrowed(p);
    }
    if let Some(share) = rest.strip_prefix(r"UNC\") {
        return Cow::Owned(PathBuf::from(format!(r"\\{share}")));
    }
    match rest.as_bytes() {
        [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic() => Cow::Owned(PathBuf::from(rest)),
        _ => Cow::Borrowed(p),
    }
}

/// An absolute Windows path [`windows_key`] refuses ONLY for its names shaped like generated 8.3 short names, cut where the mount table and the file system each answer for it ([`short_spelled`]; `crate::volumes::Table::long_spelling` spells it long).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortSpelled<'a> {
    /// The part before its first short name, in the path's own spelling (`C:\Users\` of `C:\Users\ANDREW~1\x`, `\\?\C:\` of `\\?\C:\PROGRA~1\y`).
    pub lead: &'a Path,
    /// The part through its LAST short name, verbatim, as Win32 opens it at any length (`\\?\C:\Users\ANDREW~1` of `C:\Users\ANDREW~1\x`; a `\??\` prefix spelled `\\?\`, a plain path's `/` and doubled separators as Win32 reads them): the one part the file system is asked to spell long.
    pub through: PathBuf,
    /// The names after its last short name, in order (`["x"]` of `C:\Users\ANDREW~1\x`; empty when the path ends with one).
    pub rest: Vec<&'a str>,
}

/// `p` cut for its long spelling ([`ShortSpelled`]), when names shaped like generated 8.3 short names are the ONLY reason [`windows_key`] refuses it.
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

/// The root `root` of a path [`root_of`] cut (`C:\`, `C:/`, `\\?\C:\`,
fn verbatim_root(root: &str, verbatim: bool) -> String {
    if verbatim {
        // `\\?\` and `\??\` are both four characters.
        return format!(r"\\?\{}", &root[4..]);
    }
    match root.as_bytes() {
        [a, b, ..] if (*a == b'\\' || *a == b'/') && (*b == b'\\' || *b == b'/') => {
            let share = root[2..].replace('/', r"\");
            format!(r"\\?\UNC\{}\", share.trim_end_matches('\\'))
        }
        _ => format!(r"\\?\{}\", &root[..2]),
    }
}

/// [`windows_key`] of the text `s`.
fn key_of(s: &str) -> Option<String> {
    key_with(s, false)
}

/// [`key_of`], admitting names shaped like 8.3 short names when `short`
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

/// The keyed root of `s` (`C:\`, `\\SRV\SHARE\`), the text after it, and
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

/// `X:\` keyed (`C:\`) and the text after it; a verbatim path takes only
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

/// `srv\share` keyed (`\\SRV\SHARE\`) and the text after it: both names
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

/// Whether the name `name` has the shape of a generated 8.3 short name
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

/// The characters whose simple uppercase differs from both themselves and
const SIMPLE_ONLY: &[(u32, u32, u32)] = &[
    (0x1F80, 0x1F87, 8),
    (0x1F90, 0x1F97, 8),
    (0x1FA0, 0x1FA7, 8),
    (0x1FB3, 0x1FB3, 9),
    (0x1FC3, 0x1FC3, 9),
    (0x1FF3, 0x1FF3, 9),
];

/// `c` folded as Windows folds a name to compare it blind to case, one UTF-16 unit to one (`RtlUpcaseUnicodeChar`, which a file name's key ([`windows_key`]) and an environment variable's lookup both compare by): its simple uppercase where both are one unit, else `c`.
#[must_use]
pub fn fold_case(c: char) -> char {
    fold(c)
}

/// `c`'s simple uppercase where both are one UTF-16 unit, else `c`
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
