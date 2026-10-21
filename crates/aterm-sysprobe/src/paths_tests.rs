// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows path key, held on every platform: it reads a path's text and
//! touches nothing.

use super::*;

fn key(p: &str) -> Option<String> {
    windows_key(Path::new(p))
}

fn under(p: &str, d: &str) -> Option<bool> {
    windows_under(Path::new(p), Path::new(d))
}

/// Every spelling Windows hands out for one directory keys alike: the
/// verbatim one `canonicalize` answers, the plain one a process records
/// (trailing separator, case as typed), `/` for `\`, the NT form a junction
/// stores. NEGATIVE CONTROL: `Path::starts_with` places none of them.
#[test]
fn every_spelling_of_one_directory_keys_alike() {
    let want = Some(r"C:\USERS\M6-AN\ATERM".to_owned());
    for p in [
        r"C:\Users\m6-an\aterm",
        r"\\?\C:\Users\m6-an\aterm",
        r"\??\C:\Users\m6-an\aterm",
        r"c:\USERS\M6-AN\aterm\",
        r"C:/Users//m6-an/aterm/",
        r"C:\Users\\m6-an\aterm\\",
        r"\\?\c:\users\m6-an\ATERM\",
    ] {
        assert_eq!(key(p), want, "{p}");
    }
    assert_eq!(key(r"C:\"), Some(r"C:\".to_owned()));
    assert_eq!(
        key(r"\\?\C:\"),
        Some(r"C:\".to_owned()),
        "a root keeps its separator"
    );
    assert_eq!(key("c:/"), Some(r"C:\".to_owned()));
    if cfg!(windows) {
        assert!(
            !Path::new(r"\\?\C:\Users\m6-an\aterm").starts_with(Path::new(r"C:\Users")),
            "the bug the key exists for"
        );
    }
}

/// A UNC path keys with its server and share, verbatim or not, any case; a
/// server alone or an empty name is not placed.
#[test]
fn a_unc_path_keys_by_its_server_and_share() {
    let want = Some(r"\\SRV\SHARE\X".to_owned());
    assert_eq!(key(r"\\srv\share\x"), want);
    assert_eq!(key(r"\\?\UNC\srv\Share\x"), want);
    assert_eq!(key(r"\\?\unc\SRV\share\x\"), want);
    assert_eq!(key("//srv/share/x"), want);
    assert_eq!(key(r"\\srv\share"), Some(r"\\SRV\SHARE\".to_owned()));
    assert_eq!(key(r"\\srv"), None);
    assert_eq!(key(r"\\srv\"), None);
    assert_eq!(key(r"\\?\UNC\srv"), None);
    assert_eq!(under(r"\\srv\share\y", r"\\srv\share\x"), Some(false));
    assert_eq!(
        under(r"\\srv\share2\x", r"\\srv\share"),
        None,
        "another share may be this one's folder"
    );
    assert_eq!(under(r"\\?\UNC\srv\share\x", r"\\SRV\Share"), Some(true));
}

/// Without a table, two paths are related only under one root: across two
/// drive letters, a letter and a share, or two shares, a `subst` letter, a
/// volume's second mount point or a loopback share may name the same
/// directory, so that is not placed. NEGATIVE CONTROL: under one root the
/// rule answers both ways.
#[test]
fn two_roots_are_not_related_without_a_table() {
    for (p, d) in [
        (r"S:\proj\target\debug\app", r"\\?\C:\work\proj\target"),
        (r"\\localhost\C$\work\proj\target\x", r"C:\work\proj\target"),
        (r"\\?\UNC\localhost\c$\work\x", r"C:\work"),
        (r"C:\work\x", r"\\srv\share"),
        (r"D:\Users\m6-an", r"C:\Users\m6-an"),
        (r"D:\x", r"C:\"),
    ] {
        assert_eq!(under(p, d), None, "{p} under {d}");
    }
    assert_eq!(under(r"C:\work2\x", r"\\?\c:\WORK"), Some(false));
    assert_eq!(under(r"C:\work\x", r"\\?\c:\WORK"), Some(true));
    assert_eq!(under(r"C:\x", r"C:\"), Some(true));
}

/// After a verbatim prefix nothing is normalized: `/` is a character of a
/// name, so the path is not placed; an empty name is not placed; a name
/// ending in `.` or a space is literal. A plain path's trailing-dot name is
/// trimmed by Win32 rules the key does not model: not placed.
#[test]
fn a_verbatim_path_is_read_literally() {
    assert_eq!(key(r"\\?\C:\a/b"), None, "`/` after a verbatim prefix");
    assert_eq!(key(r"\\?\UNC\srv/x\share"), None);
    assert_eq!(key(r"\\?\C:/a"), None);
    assert_eq!(key(r"\\?\C:\a\\b"), None, "an empty verbatim name");
    assert_eq!(key(r"\\?\C:\a.\b "), Some(r"C:\A.\B ".to_owned()));
    assert_eq!(key(r"C:\a.\b"), None);
    assert_eq!(key(r"C:\a\b "), None);
    assert_eq!(
        key("C:/a/b"),
        Some(r"C:\A\B".to_owned()),
        "`/` separates a plain path"
    );
}

/// What is not a placed absolute path answers `None`: relative,
/// drive-relative, root-relative, `.` and `..`, the device namespace, a
/// volume GUID path (the table places that one), a drive's device.
#[test]
fn what_cannot_be_placed_answers_none() {
    for p in [
        "",
        "rel",
        r"rel\x",
        "C:",
        "C:x",
        r"\x",
        "/x",
        r"C:\a\..\b",
        r"C:\a\.\b",
        r"\\?\C:\a\..\b",
        r"\\.\C:\x",
        r"\\.\pipe\x",
        "//./C:/x",
        "//?/C:/x",
        r"\\?/C:\x",
        r"\\?\GLOBALROOT\Device\HarddiskVolume3\x",
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}\x",
        r"\\?\C:",
        r"1:\x",
    ] {
        assert_eq!(key(p), None, "{p:?}");
        assert_eq!(under(p, r"C:\"), None, "{p:?}");
        assert_eq!(under(r"C:\x", p), None, "{p:?}");
    }
}

/// Case folds character by character to a simple uppercase of one UTF-16
/// unit, as NTFS's upcase table does: `ä` is `Ä`, `ß` (whose uppercase is two
/// letters) stays, and a character outside the Basic Multilingual Plane keeps
/// its case.
#[test]
fn case_folds_as_ntfs_upcases_a_name() {
    assert_eq!(key(r"C:\Ärger\ärger"), Some(r"C:\ÄRGER\ÄRGER".to_owned()));
    assert_eq!(under(r"c:\Σίσυφος\x", r"C:\σΊΣΥΦΟΣ"), Some(true));
    assert_eq!(key(r"C:\straße"), Some(r"C:\STRAßE".to_owned()));
    assert_eq!(under(r"C:\STRASSE", r"C:\straße"), Some(false));
    assert_eq!(key("C:\\\u{10428}"), Some("C:\\\u{10428}".to_owned()));
    assert_eq!(under("C:\\\u{10400}", "C:\\\u{10428}"), Some(false));
}

/// The fold is Unicode's SIMPLE uppercase: each Greek letter with
/// ypogegrammeni (`ᾀ` U+1F80) folds to its prosgegrammeni form (`ᾈ`
/// U+1F88), as NTFS folds it — 27 characters whose full uppercase is two
/// letters (`ἈΙ`). NEGATIVE CONTROL: `char::to_uppercase` (the full
/// mapping) spells each of them as two characters, which a fold of one unit
/// cannot use; a letter of the block with no simple uppercase (`ᾲ`) keeps
/// its case.
#[test]
fn case_folds_by_the_simple_uppercase() {
    let pairs = (0x1F80..=0x1F87)
        .chain(0x1F90..=0x1F97)
        .chain(0x1FA0..=0x1FA7)
        .map(|u| (u, u + 8))
        .chain([(0x1FB3, 0x1FBC), (0x1FC3, 0x1FCC), (0x1FF3, 0x1FFC)]);
    let mut n = 0;
    for (lower, upper) in pairs {
        let (l, u) = (
            char::from_u32(lower).unwrap(),
            char::from_u32(upper).unwrap(),
        );
        assert_eq!(l.to_uppercase().count(), 2, "U+{lower:04X}: full mapping");
        assert_eq!(key(&format!(r"C:\a{l}b")), key(&format!(r"C:\a{u}b")));
        assert_eq!(
            under(&format!(r"C:\x{l}\target"), &format!(r"C:\X{u}")),
            Some(true),
            "U+{lower:04X}"
        );
        n += 1;
    }
    assert_eq!(n, 27);
    assert_eq!(key("C:\\\u{1FB2}"), Some("C:\\\u{1FB2}".to_owned()));
}

/// Under is component by component, at the directory itself too; a root
/// holds everything on its drive and nothing on another.
#[test]
fn under_is_component_by_component() {
    let dir = r"C:\Users\m6-an";
    assert_eq!(
        under(r"\\?\C:\USERS\m6-an\aterm\target\debug", dir),
        Some(true)
    );
    assert_eq!(
        under(r"c:\users\M6-AN\", dir),
        Some(true),
        "the directory itself"
    );
    assert_eq!(
        under(r"C:\Users\m6-anx", dir),
        Some(false),
        "a component, not a prefix"
    );
    assert_eq!(under(r"C:\Users", dir), Some(false), "above it");
    assert_eq!(under(r"C:\anything\at\all", r"\\?\C:\"), Some(true));
    assert_eq!(under(r"c:\", r"C:\Users"), Some(false), "the root, above");
    assert!(key_under(r"C:\A\B", r"C:\A"));
    assert!(key_under(r"C:\A", r"C:\A"));
    assert!(key_under(r"C:\A", r"C:\"));
    assert!(!key_under(r"C:\AB", r"C:\A"));
    assert!(!key_under(r"C:\A", r"C:\A\B"));
}

/// A name shaped like a generated 8.3 short name names ANOTHER name's file
/// (`C:\PROGRA~1` is `C:\Program Files`; this box generates them), so it is
/// not placed — in any case, verbatim or not, with an extension, in the
/// hashed form — rather than keyed apart from its long name: a caller that
/// may hold one canonicalizes first or compares file ids. NEGATIVE CONTROL:
/// a `~` in a name of any other shape is a plain character.
#[test]
fn a_short_name_is_not_placed() {
    assert_eq!(under(r"C:\PROGRA~1\x", r"C:\Program Files"), None);
    assert_eq!(under(r"C:\Users\M6-AN~1\x", r"C:\Users\m6-an"), None);
    assert_eq!(under(r"C:\Users\m6-an\x", r"c:\users\m6-an~1"), None);
    for short in [
        r"C:\PROGRA~1",
        r"c:\progra~1\x",
        r"\\?\C:\Users\ANDREW~1\AppData",
        r"C:\x\ABCDE~10.TXT",
        r"C:\x\AB12CD~1",
        r"C:\x\a~1.c",
        r"C:\x\~1",
        r"\\srv\share\LONGNA~2\x",
    ] {
        assert_eq!(key(short), None, "{short}");
    }
    for plain in [
        r"C:\x\foo~bar",
        r"C:\x\a~1b",
        r"C:\x\averylongname~1",
        r"C:\x\x~1.json5",
        r"C:\x\a.b~1.c",
        r"C:\x\~$report.docx",
        r"C:\x\~",
        r"C:\x\abc~",
    ] {
        assert!(key(plain).is_some(), "{plain}");
    }
}

/// A path refused ONLY for its short-shaped names is short-spelled, cut in
/// three: the part before the first of them is what keys — the root, or the
/// long names leading to it, in the path's own spelling (verbatim, a share,
/// a doubled separator) — what the mount table places before anything opens
/// the path; the part through the LAST of them, verbatim with Win32's own
/// normalization (a `\??\` prefix, `/` and doubled separators), is all the
/// file system is asked about; the names after it are kept as they are, a
/// trailing separator dropped. NEGATIVE CONTROLS: a path the key places is
/// not short-spelled, nor is one refused for another reason as well
/// (relative, `..`, a trailing dot, the device namespace, a verbatim `/`),
/// nor one with no short name.
#[test]
fn only_a_path_refused_for_its_short_names_is_short_spelled() {
    let lead =
        |p: &str| short_spelled(Path::new(p)).map(|s| s.lead.to_str().expect("utf-8").to_owned());
    for (path, want, through, rest) in [
        (r"C:\PROGRA~1", r"C:\", r"\\?\C:\PROGRA~1", &[][..]),
        (
            r"C:\Users\ANDREW~1\x",
            r"C:\Users\",
            r"\\?\C:\Users\ANDREW~1",
            &["x"][..],
        ),
        (
            r"c:\users\andrew~1\PROJEC~2\y\",
            r"c:\users\",
            r"\\?\c:\users\andrew~1\PROJEC~2",
            &["y"][..],
        ),
        (
            r"\\?\C:\Users\ANDREW~1\AppData",
            r"\\?\C:\Users\",
            r"\\?\C:\Users\ANDREW~1",
            &["AppData"][..],
        ),
        (
            r"\??\C:\PROGRA~1\a\b",
            r"\??\C:\",
            r"\\?\C:\PROGRA~1",
            &["a", "b"][..],
        ),
        (
            r"\\srv\share\LONGNA~2\x",
            r"\\srv\share\",
            r"\\?\UNC\srv\share\LONGNA~2",
            &["x"][..],
        ),
        (
            r"\\?\UNC\srv\share\LONGNA~2",
            r"\\?\UNC\srv\share\",
            r"\\?\UNC\srv\share\LONGNA~2",
            &[][..],
        ),
        (
            r"C:\a\\b\AB12CD~1\x",
            r"C:\a\\b\",
            r"\\?\C:\a\b\AB12CD~1",
            &["x"][..],
        ),
        (
            r"C:/a/ABCDE~10.TXT",
            r"C:/a/",
            r"\\?\C:\a\ABCDE~10.TXT",
            &[][..],
        ),
    ] {
        let cut = short_spelled(Path::new(path)).unwrap_or_else(|| panic!("{path}"));
        assert_eq!(cut.lead, Path::new(want), "{path}");
        assert!(key(want).is_some(), "{path}: its lead keys");
        assert_eq!(cut.through, Path::new(through), "{path}");
        assert_eq!(cut.rest, rest, "{path}");
    }
    for not in [
        r"C:\Users\andrew\x",
        r"C:\x\foo~bar",
        r"PROGRA~1\x",
        r"C:PROGRA~1",
        r"C:\PROGRA~1\..\x",
        r"C:\PROGRA~1\x.",
        r"\\.\C:\PROGRA~1",
        r"\\?\C:\PROGRA~1\a/b",
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}\PROGRA~1",
    ] {
        assert_eq!(lead(not), None, "{not}");
    }
}

/// The platform's rule: the key on Windows, `starts_with` elsewhere.
#[test]
fn is_under_takes_the_platforms_rule() {
    if cfg!(windows) {
        assert_eq!(
            is_under(Path::new(r"\\?\C:\Users\x"), Path::new(r"c:\users")),
            Some(true)
        );
        assert_eq!(
            is_under(Path::new(r"C:\Users\x"), Path::new(r"C:\Use")),
            Some(false)
        );
        assert_eq!(is_under(Path::new("rel"), Path::new(r"C:\")), None);
    } else {
        assert_eq!(
            is_under(Path::new("/home/x/y"), Path::new("/home/x")),
            Some(true)
        );
        assert_eq!(
            is_under(Path::new("/home/X/y"), Path::new("/home/x")),
            Some(false)
        );
        assert_eq!(
            is_under(Path::new("/home/xy"), Path::new("/home/x")),
            Some(false)
        );
    }
}

/// The display spelling drops a verbatim drive or UNC prefix and nothing
/// else: a volume GUID path has no other spelling, and a `/` after a verbatim
/// prefix would read as a separator once plain.
#[test]
fn plain_drops_only_a_verbatim_drive_or_unc_prefix() {
    let plain_of = |p: &str| plain(Path::new(p)).into_owned();
    assert_eq!(plain_of(r"\\?\C:\Users\x"), PathBuf::from(r"C:\Users\x"));
    assert_eq!(
        plain_of(r"\\?\UNC\srv\share\x"),
        PathBuf::from(r"\\srv\share\x")
    );
    for kept in [
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}\x",
        r"\\?\C:\a/b",
        r"\\?\GLOBALROOT\Device\x",
        r"C:\Users\x",
        "/home/x",
    ] {
        assert_eq!(plain_of(kept), PathBuf::from(kept), "{kept}");
    }
}

/// Every character of the Basic Multilingual Plane keys as the system's own
/// upcase table (`RtlUpcaseUnicodeChar`, the table NTFS formats a volume's
/// `$UpCase` from) folds it: two names NTFS opens as one file never key
/// apart. Pure — no file is touched — and its own negative control: the
/// full uppercase mapping fails it at U+1F80.
#[cfg(windows)]
#[test]
fn every_bmp_character_keys_as_the_system_upcases_it() {
    let mut apart = Vec::new();
    for unit in (0..=0xFFFF_u16).filter(|u| !(0xD800..=0xDFFF).contains(u)) {
        let (c, up) = (
            char::from_u32(u32::from(unit)).unwrap(),
            char::from_u32(u32::from(crate::win_paths::upcase(unit))).unwrap(),
        );
        if key(&format!(r"C:\a{c}b")) != key(&format!(r"C:\a{up}b")) {
            apart.push(format!("U+{unit:04X}"));
        }
    }
    assert!(apart.is_empty(), "keyed apart from their upcase: {apart:?}");
}

/// NTFS itself opens each of the 27 simple-uppercase pairs as one name: a
/// directory made with the small letter is found by the capital one, in a
/// scratch directory on the temp volume. NEGATIVE CONTROL: `ß` and `SS`,
/// which the key keeps apart, are two names there too.
#[cfg(windows)]
#[test]
fn ntfs_opens_a_simple_uppercase_pair_as_one_name() {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let pairs = (0x1F80..=0x1F87_u32)
        .chain(0x1F90..=0x1F97)
        .chain(0x1FA0..=0x1FA7)
        .map(|u| (u, u + 8))
        .chain([(0x1FB3, 0x1FBC), (0x1FC3, 0x1FCC), (0x1FF3, 0x1FFC)]);
    for (lower, upper) in pairs {
        let (l, u) = (
            char::from_u32(lower).unwrap(),
            char::from_u32(upper).unwrap(),
        );
        let made = dir.path().join(format!("x{l}"));
        std::fs::create_dir(&made).expect("a scratch name");
        assert!(
            dir.path().join(format!("x{u}")).is_dir(),
            "U+{lower:04X} opened as U+{upper:04X}"
        );
        assert_eq!(
            windows_under(&made, &dir.path().join(format!("x{u}"))),
            Some(true)
        );
    }
    std::fs::create_dir(dir.path().join("straße")).expect("a scratch name");
    assert!(!dir.path().join("STRASSE").exists(), "two names to NTFS");
}
