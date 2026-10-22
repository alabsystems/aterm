// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PATHS — the one rule that places a path under a directory across the
//! spellings Windows hands out for one location. Pure: no path is touched,
//! so it is compiled and tested on every platform.
//!
//! Windows names one directory several ways. `std::fs::canonicalize`
//! answers `\\?\C:\Users\x` (verbatim); a process's own record (its PEB
//! working directory, its image, a module) says `C:\Users\X\` — case as
//! typed, a trailing separator; the mount manager lists `C:\`. To
//! `Path::starts_with` a verbatim prefix (`Prefix::VerbatimDisk`) never
//! equals a plain one (`Prefix::Disk`), and `Normal` components compare
//! case-sensitively while NTFS does not. Every comparison across two of
//! those sources then reads "not under" — which a fence that asks "does
//! anyone use this directory" reads as "nobody does": a false UNUSED deletes
//! someone's live build. So every cross-source comparison goes through this
//! module's key ([`windows_key`], [`windows_under`]), or through file
//! identity (`crate::volumes::file_id`) where both sides exist.
//!
//! A path the key cannot place answers `None`, and every caller reads that
//! as IN USE / unknown — never as "not under".
//!
//! The rule with the machine's mount table at hand is
//! `crate::volumes::Table::is_under`, and every cross-source comparison on
//! Windows goes through it: it follows a `subst` letter to the directory it
//! names, relates two mount points of one volume, and answers `None` where
//! only a server could relate two paths. This module's [`windows_under`] is
//! the rule without a table: it relates two paths under ONE root (one drive
//! letter, one `\\server\share`) and answers `None` across two roots.
//!
//! # What the key does not see
//!
//! * A drive letter is a name, not a volume: `S:\x` of `subst S: C:\work`
//!   is `C:\work\x`, a mapped letter or a share can be the local disk again
//!   (`net use Z: \\localhost\C$`, `\\localhost\C$\work`), and one volume
//!   can be mounted at a letter and at folders (`D:\x` is `C:\mnt\d\x`). So
//!   two roots answer `None` here; the table relates them. Two folders of
//!   one drive can be one volume's two mount points (`C:\mnt\d` and
//!   `C:\alias`): under one root this key reads them apart, so a caller
//!   that may meet one uses the table, or compares file ids.
//! * An 8.3 short name (`C:\PROGRA~1`, `C:\Users\ANDREW~1`) names its long
//!   name's file: a component of that shape answers `None` rather than key
//!   apart from the long spelling. A path a process or a shell reported is
//!   spelled long first by `crate::volumes::Table::long_spelling` (on a
//!   local volume only, [`short_spelled`] naming the part the table places
//!   and the part the file system is asked about), or compared by file id;
//!   one that cannot be expanded stays `None`.
//! * Hard links, junctions and symbolic links: the key is lexical. A path a
//!   process records (its working directory, its image) keeps the spelling
//!   it was given; canonicalize it, or compare file identity, which sees
//!   through them.
//! * A directory made case-sensitive (`fsutil file setCaseSensitiveInfo`,
//!   WSL) still keys case-insensitively: two names it holds apart read as
//!   one, which places a path UNDER a directory it is not under — the held
//!   side, never the deleting one.
//! * A volume named by its GUID (`\\?\Volume{…}\x`) names no letter: `None`
//!   here; `crate::volumes::mount_of` places it through the mount table.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// The comparison key of an absolute Windows path, or `None` where it
/// cannot be placed.
///
/// * The verbatim prefixes go: `\\?\C:\x` and `\??\C:\x` key as `C:\x`,
///   `\\?\UNC\srv\share\x` as `\\srv\share\x`. The drive letter is
///   uppercased; a UNC path needs both a server and a share.
/// * After a verbatim prefix nothing is normalized, so `/` is a literal
///   character of a name, not a separator: such a path answers `None`
///   (Win32 never hands one out; the NT layer refuses the name). Elsewhere
///   `/` is a separator, as Win32 reads it.
/// * Trailing separators go (`C:\x\` is `C:\x`), and so do repeated ones
///   in a plain path (`C:\a\\b` is `C:\a\b`, as Win32 collapses them); an
///   empty name inside a verbatim path answers `None`.
/// * Each character is folded to its SIMPLE uppercase (Unicode's one-to-one
///   mapping, never the full one that spells `ß` as `SS`) where both are
///   one UTF-16 unit, as NTFS's `$UpCase` table folds names unit by unit:
///   `ß` keeps its case, `ᾀ` is `ᾈ`, and characters outside the Basic
///   Multilingual Plane keep their case. A character NTFS folds is
///   folded alike (a Windows test holds every BMP character to the
///   system's own upcase table); where this folds a character NTFS's table
///   (an older one, written when the volume was formatted) does not, two
///   names read as one: the held side.
/// * `None` for a relative, drive-relative (`C:x`) or root-relative (`\x`)
///   path; a `.` or `..` component; a plain-path name ending in `.` or a
///   space (Win32 trims those by rules this key does not model; a verbatim
///   name keeps them); a name shaped like a generated 8.3 short name
///   (`PROGRA~1`, which names another name's file); the device namespace
///   (`\\.\`, `\\?\GLOBALROOT`, `//?/`); a volume GUID path; a path that is
///   not Unicode.
///
/// The key of a root keeps its separator (`C:\`, `\\SRV\SHARE\`); every
/// other key has none at its end (`C:\USERS\X`).
#[must_use]
pub fn windows_key(p: &Path) -> Option<String> {
    key_of(p.to_str()?)
}

/// Whether `path` is `dir` or lies under it, by [`windows_key`], without a
/// mount table. `Some(false)` only for two paths under one root (one drive
/// letter, one `\\server\share`): across two roots — `S:\x` and `C:\work`,
/// `\\localhost\C$\x` and `C:\x` — only the table (a `subst` letter, a
/// volume's other mount points) or the server could relate them, so that
/// is `None`, as is a path either side cannot place: the caller holds the
/// path as IN USE / unknown, never as "not under". With the table at hand,
/// `crate::volumes::Table::is_under` answers more.
#[must_use]
pub fn windows_under(path: &Path, dir: &Path) -> Option<bool> {
    let (path, dir) = (windows_key(path)?, windows_key(dir)?);
    if key_under(&path, &dir) {
        return Some(true);
    }
    (root(&path) == root(&dir)).then_some(false)
}

/// A [`windows_key`]'s root (`C:\`, `\\SRV\SHARE\`): through its first `\`,
/// or a UNC key's fourth.
fn root(key: &str) -> &str {
    let nth = if key.starts_with(r"\\") { 3 } else { 0 };
    let end = key
        .match_indices('\\')
        .nth(nth)
        .map_or(key.len(), |(i, _)| i + 1);
    &key[..end]
}

/// Whether key `key` is `dir` or lies under it, component by component
/// (`C:\AB` is not under `C:\A`). Both are [`windows_key`]s.
#[inline]
#[must_use]
pub fn key_under(key: &str, dir: &str) -> bool {
    key.strip_prefix(dir)
        .is_some_and(|rest| rest.is_empty() || dir.ends_with('\\') || rest.starts_with('\\'))
}

/// Whether `path` is `dir` or lies under it, by this platform's rule without
/// a mount table: [`windows_under`] on Windows, `Path::starts_with` (always
/// placed) elsewhere. `None`: it cannot be placed — IN USE / unknown. With
/// the table at hand, `crate::volumes::Table::is_under`.
#[must_use]
pub fn is_under(path: &Path, dir: &Path) -> Option<bool> {
    if cfg!(windows) {
        windows_under(path, dir)
    } else {
        Some(path.starts_with(dir))
    }
}

/// `p` without the verbatim prefix `std::fs::canonicalize` answers with on
/// Windows, for DISPLAY: `\\?\C:\x` reads `C:\x` and `\\?\UNC\srv\share\x`
/// reads `\\srv\share\x`. Any other path comes back as it is: a volume GUID
/// path (it has no other spelling), and a verbatim path holding a `/` (a
/// literal character there, which would read as a separator once plain).
///
/// NEVER pass the answer to an operation or keep it as a row's operational
/// path: past `MAX_PATH` (`LongPathsEnabled` is 0 on most machines) only the
/// verbatim spelling opens, and a name ending in `.` or a space means
/// another file once plain.
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

/// An absolute Windows path [`windows_key`] refuses ONLY for its names
/// shaped like generated 8.3 short names, cut where the mount table and the
/// file system each answer for it ([`short_spelled`];
/// `crate::volumes::Table::long_spelling` spells it long).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortSpelled<'a> {
    /// The part before its first short name, in the path's own spelling
    /// (`C:\Users\` of `C:\Users\ANDREW~1\x`, `\\?\C:\` of
    /// `\\?\C:\PROGRA~1\y`). It keys, so the mount table says which volume
    /// the rest lies on before anything opens it.
    pub lead: &'a Path,
    /// The part through its LAST short name, verbatim, as Win32 opens it at
    /// any length (`\\?\C:\Users\ANDREW~1` of `C:\Users\ANDREW~1\x`; a
    /// `\??\` prefix spelled `\\?\`, a plain path's `/` and doubled
    /// separators as Win32 reads them): the one part the file system is
    /// asked to spell long. It must exist; nothing after it is asked.
    pub through: PathBuf,
    /// The names after its last short name, in order (`["x"]` of
    /// `C:\Users\ANDREW~1\x`; empty when the path ends with one). Each is
    /// its own long name as the key reads names, so it is appended as it
    /// is, and need not exist: a directory a tab names before it is made,
    /// or after it is removed, is spelled long all the same.
    pub rest: Vec<&'a str>,
}

/// `p` cut for its long spelling ([`ShortSpelled`]), when names shaped like
/// generated 8.3 short names are the ONLY reason [`windows_key`] refuses it.
/// `None` for a path the key places, and for one it refuses for any other
/// reason too (relative, `..`, the device namespace, a trailing dot, a
/// volume GUID path): no expansion places those. Pure.
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
/// `\??\C:\`, `\\srv\share\`, `\\?\UNC\srv\share\`), spelled verbatim with
/// its separator: `\\?\C:\`, `\\?\UNC\srv\share\`.
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
/// ([`short_spelled`]: the key such a path would have once it is spelled
/// long — never a key to compare by).
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
/// whether `s` is verbatim (`\\?\`, `\??\`: nothing after it is
/// normalized). `None` for anything but an absolute drive or UNC path.
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
        // `\\.\`, `\\?/`, `//?/`: the device namespace, never a file's name.
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
/// `\` after the colon.
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
/// present and non-empty.
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
/// (`PROGRA~1`, `ABCDE~10.TXT`, `AB12CD~1`): at most eight characters, the
/// last of them `~` and digits, then at most one dot and three more. A file
/// system that makes short names opens such a name as ANOTHER name's file
/// (`C:\PROGRA~1` is `C:\Program Files`), whatever case or prefix spells
/// it, so the key cannot place it ([`windows_key`]). A name of that shape
/// that is its own long name is held too: the held side. Any other `~`
/// (`foo~bar`, `a~1b`, a longer `averylongname~1`) is a plain character.
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
/// what `char::to_uppercase` answers: Rust's mapping is Unicode's FULL one
/// (`SpecialCasing.txt`), which spells each of these as two characters
/// (`ᾀ` as `ἈΙ`), while their simple uppercase — the one NTFS's `$UpCase`
/// folds by — is one character: the Greek letters with ypogegrammeni, each
/// to its prosgegrammeni form. Every other character with a multi-character
/// full uppercase (`ß`, `ŉ`, `ǰ`, `ﬀ`, …) has no simple uppercase: it keeps
/// its case. `(first, last, offset)`: each of `first..=last` folds to itself
/// plus `offset`. Held to the system's own table by a Windows test.
const SIMPLE_ONLY: &[(u32, u32, u32)] = &[
    (0x1F80, 0x1F87, 8),
    (0x1F90, 0x1F97, 8),
    (0x1FA0, 0x1FA7, 8),
    (0x1FB3, 0x1FB3, 9),
    (0x1FC3, 0x1FC3, 9),
    (0x1FF3, 0x1FF3, 9),
];

/// `c` folded as Windows folds a name to compare it blind to case, one
/// UTF-16 unit to one (`RtlUpcaseUnicodeChar`, which a file name's key
/// ([`windows_key`]) and an environment variable's lookup both compare by):
/// its simple uppercase where both are one unit, else `c`. Held to the
/// system's own table, every BMP character, by a Windows test.
#[must_use]
pub fn fold_case(c: char) -> char {
    fold(c)
}

/// `c`'s simple uppercase where both are one UTF-16 unit, else `c`
/// ([`windows_key`]): `char::to_uppercase` where that is one character,
/// [`SIMPLE_ONLY`] where it is two or more.
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
