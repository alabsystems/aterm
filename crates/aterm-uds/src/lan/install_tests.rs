// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeMap;

/// A universal file of `slices` (32-bit fat table, each slice 16-byte aligned).
fn universal(slices: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&FAT_MAGIC.to_be_bytes());
    out.extend_from_slice(&(slices.len() as u32).to_be_bytes());
    let mut at = 8 + 20 * slices.len();
    let mut body = Vec::new();
    for slice in slices {
        let pad = (16 - at % 16) % 16;
        at += pad;
        body.resize(body.len() + pad, 0);
        for word in [le(slice, 4), 0, at as u32, slice.len() as u32, 4] {
            out.extend_from_slice(&word.to_be_bytes());
        }
        body.extend_from_slice(slice);
        at += slice.len();
    }
    out.extend_from_slice(&body);
    out
}

/// The flat `<key>`/value pairs of a plist dict, read back from its text: each value a string,
/// `true`, or a `[..]` list of strings.
fn keys(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut lines = text.lines().map(str::trim).peekable();
    let inner = |line: &str, tag: &str| {
        line.strip_prefix(&format!("<{tag}>"))
            .and_then(|rest| rest.strip_suffix(&format!("</{tag}>")))
            .map(|text| {
                text.replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&apos;", "'")
                    .replace("&amp;", "&")
            })
    };
    while let Some(line) = lines.next() {
        let Some(key) = inner(line, "key") else {
            continue;
        };
        let value = match lines.next().unwrap() {
            "<true/>" => "true".to_owned(),
            "<array>" => {
                let mut items = Vec::new();
                for item in lines.by_ref() {
                    if item == "</array>" {
                        break;
                    }
                    items.push(inner(item, "string").unwrap());
                }
                format!("[{}]", items.join("|"))
            }
            other => inner(other, "string").unwrap(),
        };
        out.insert(key, value);
    }
    out
}

/// The plist's keys are the job the design names: a root system daemon (no `UserName`)
/// running the clone's executable with `lan serve --source <the source>`, kept alive, its
/// output in the broker's log. A source path with XML's own characters reads back whole.
#[test]
fn the_plist_runs_the_clone_as_root_with_its_source_named() {
    let source = Path::new("/Applications/a&b <dev>.app");
    let text = plist_text(source);
    let keys = keys(&text);
    let want = [
        ("Label", super::super::LABEL.to_owned()),
        (
            "ProgramArguments",
            format!(
                "[{}|lan|serve|--source|/Applications/a&b <dev>.app]",
                super::super::IMAGE_EXECUTABLE
            ),
        ),
        ("RunAtLoad", "true".to_owned()),
        ("KeepAlive", "true".to_owned()),
        ("ProcessType", "Standard".to_owned()),
        (
            "AssociatedBundleIdentifiers",
            "[com.aterm.aterm]".to_owned(),
        ),
        ("StandardOutPath", super::super::LOG_PATH.to_owned()),
        ("StandardErrorPath", super::super::LOG_PATH.to_owned()),
    ];
    assert_eq!(
        keys,
        want.iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
        "{text}"
    );
    assert!(!text.contains("UserName"), "{text}");
    assert_ne!(text, plist_text(Path::new("/Applications/aterm.app")));
}

/// Thin and universal files read every CodeDirectory of every slice, from bytes and from a
/// file alike. NEGATIVE CONTROLS: a file with no signature, a non-Mach-O, and one cut short
/// are refused, never read as some identity.
#[test]
fn code_directories_are_read_from_each_slice() {
    let arm = test_image(0x0100_000c, &[b"sha1 directory", b"sha256 directory"]);
    let x86 = test_image(0x0100_0007, &[b"x86 directory"]);
    let thin = CodeDirectories::of_bytes(&arm).unwrap();
    assert_eq!(
        thin,
        CodeDirectories(vec![
            (
                0x0100_000c,
                [
                    &CODE_DIRECTORY.to_be_bytes()[..],
                    &22_u32.to_be_bytes(),
                    b"sha1 directory"
                ]
                .concat()
            ),
            (
                0x0100_000c,
                [
                    &CODE_DIRECTORY.to_be_bytes()[..],
                    &24_u32.to_be_bytes(),
                    b"sha256 directory"
                ]
                .concat()
            ),
        ])
    );
    let fat = CodeDirectories::of_bytes(&universal(&[arm.clone(), x86])).unwrap();
    assert_eq!(fat.0.len(), 3);
    assert_eq!(fat.0[..2], thin.0[..]);
    assert_eq!(fat.0[2].0, 0x0100_0007);

    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("aterm");
    std::fs::write(&path, &arm).unwrap();
    assert_eq!(CodeDirectories::of_file(&path).unwrap(), thin);
    let other = test_image(0x0100_000c, &[b"sha1 directorz", b"sha256 directory"]);
    assert_ne!(CodeDirectories::of_bytes(&other).unwrap(), thin);

    let mut unsigned = arm.clone();
    unsigned[32..36].copy_from_slice(&0x19_u32.to_le_bytes());
    for (bytes, kind) in [
        (unsigned, io::ErrorKind::InvalidData),
        (b"#!/bin/sh\necho hi\n".to_vec(), io::ErrorKind::InvalidData),
        (arm[..arm.len() - 1].to_vec(), io::ErrorKind::UnexpectedEof),
    ] {
        assert_eq!(CodeDirectories::of_bytes(&bytes).unwrap_err().kind(), kind);
    }
}

/// Offsets and lengths the file names are checked, never trusted: a 64-bit universal entry
/// at the end of `u64`, and a CodeDirectory that claims more than the file holds, read as a
/// file that ends early, from bytes and from a file alike, with nothing allocated for them.
#[test]
fn offsets_past_the_file_read_as_short_never_overflow() {
    let mut wide = Vec::new();
    wide.extend_from_slice(&FAT_MAGIC_64.to_be_bytes());
    wide.extend_from_slice(&1_u32.to_be_bytes());
    for word in [0x0100_000c_u32, 0] {
        wide.extend_from_slice(&word.to_be_bytes());
    }
    wide.extend_from_slice(&(u64::MAX - 1).to_be_bytes());
    wide.extend_from_slice(&64_u64.to_be_bytes());
    wide.extend_from_slice(&[0; 8]);

    let mut claims = test_image(0x0100_000c, &[b"this build"]);
    // The signature's length in the load command, then the blob's own length.
    claims[44..48].copy_from_slice(&u32::MAX.to_le_bytes());
    let blob = 48 + 12 + 8;
    claims[blob + 4..blob + 8].copy_from_slice(&MAX_DIRECTORY.to_be_bytes());

    let dir = aterm_tempfile::tempdir().unwrap();
    for (name, bytes) in [("wide", wide), ("claims", claims)] {
        assert_eq!(
            CodeDirectories::of_bytes(&bytes).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof,
            "{name}"
        );
        let path = dir.path().join(name);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            CodeDirectories::of_file(&path).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof,
            "{name}"
        );
    }
}

/// A real signed executable (this test binary, which Apple's linker signs ad hoc on arm64)
/// reads the same from its file's headers as from its whole bytes, and names no signer.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn a_real_executable_reads_the_same_from_its_file_and_its_bytes() {
    let exe = std::env::current_exe().unwrap();
    let from_file = CodeDirectories::of_file(&exe).unwrap();
    use std::io::Seek as _;
    let mut held = std::fs::File::open(&exe).unwrap();
    held.seek(std::io::SeekFrom::Start(7)).unwrap();
    assert_eq!(CodeDirectories::of_held(&held).unwrap(), from_file);
    assert_eq!(held.stream_position().unwrap(), 7);
    assert!(!from_file.0.is_empty());
    assert_eq!(
        from_file.signer(),
        None,
        "the linker's signature names no team"
    );
    assert_eq!(
        from_file,
        CodeDirectories::of_bytes(&std::fs::read(&exe).unwrap()).unwrap()
    );
    let hash = crate::process::read_code_directory_hash(std::process::id() as i32).unwrap();
    assert!(from_file.matches_loaded_hash(hash));
    assert_eq!(from_file.bundle_bound(hash), Some(false));
    let mut other = hash;
    other[0] ^= 1;
    assert!(!from_file.matches_loaded_hash(other));
    assert_eq!(from_file.bundle_bound(other), None);
}

#[cfg(target_os = "macos")]
#[test]
fn bundle_binding_requires_bounded_special_slots_of_the_loaded_directory() {
    let mut bytes = vec![0; 44 + 3 * 32];
    let size = bytes.len() as u32;
    bytes[16..20].copy_from_slice(&size.to_be_bytes());
    bytes[24..28].copy_from_slice(&3_u32.to_be_bytes());
    bytes[36] = 32;
    bytes[37] = 2;
    let judge = |bytes: &[u8]| {
        let hash = directory_hash(2, bytes).unwrap();
        CodeDirectories(vec![(0, bytes.to_vec())]).bundle_bound(hash)
    };
    assert_eq!(judge(&bytes), Some(false));
    // Requirements are embedded in the signature, whereas Info.plist and the
    // resource envelope require their external bundle layout.
    bytes[44 + 32] = 1;
    assert_eq!(judge(&bytes), Some(false));
    bytes[44] = 1;
    assert_eq!(judge(&bytes), Some(true));
    bytes[44] = 0;
    bytes[44 + 64] = 1;
    assert_eq!(judge(&bytes), Some(true));
    bytes[16..20].copy_from_slice(&1_u32.to_be_bytes());
    assert_eq!(judge(&bytes), None);
}

#[cfg(target_os = "macos")]
#[test]
fn native_directory_hashes_match_standard_vectors() {
    let sha1 = [
        0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50, 0xc2,
        0x6c, 0x9c, 0xd0, 0xd8, 0x9d,
    ];
    let sha256 = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3,
    ];
    let sha384 = [
        0xcb, 0x00, 0x75, 0x3f, 0x45, 0xa3, 0x5e, 0x8b, 0xb5, 0xa0, 0x3d, 0x69, 0x9a, 0xc6, 0x50,
        0x07, 0x27, 0x2c, 0x32, 0xab,
    ];
    for (kind, expected) in [(1, sha1), (2, sha256), (3, sha256), (4, sha384)] {
        assert_eq!(directory_hash(kind, b"abc"), Some(expected));
    }
    assert_eq!(directory_hash(0, b"abc"), None);
}

struct Fixed {
    files: BTreeMap<PathBuf, io::Result<Vec<u8>>>,
}

impl Fixed {
    fn new(entries: Vec<(&str, io::Result<Vec<u8>>)>) -> Self {
        Self {
            files: entries
                .into_iter()
                .map(|(path, bytes)| (PathBuf::from(path), bytes))
                .collect(),
        }
    }
}

impl Files for Fixed {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        match self.files.get(path) {
            Some(Ok(bytes)) => Ok(bytes.clone()),
            Some(Err(e)) => Err(io::Error::from(e.kind())),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }

    fn code_directories(&self, path: &Path) -> io::Result<CodeDirectories> {
        CodeDirectories::of_bytes(&self.read(path)?)
    }
}

/// A CodeDirectory blob's body (what follows its magic and length) of `version` and `flags`
/// naming `identifier` and, at a nonzero `teamOffset`, `team`.
fn signed(version: u32, flags: u32, identifier: &str, team: Option<&str>) -> Vec<u8> {
    let ident_at = 52_u32;
    let team_at = team.map_or(0, |_| ident_at + identifier.len() as u32 + 1);
    let mut body = Vec::new();
    for word in [version, flags, 0, ident_at, 0, 0, 0, 0, 0, 0, team_at] {
        body.extend_from_slice(&word.to_be_bytes());
    }
    body.extend_from_slice(identifier.as_bytes());
    body.push(0);
    if let Some(team) = team {
        body.extend_from_slice(team.as_bytes());
        body.push(0);
    }
    body
}

/// A team signature names its identifier and team; an ad-hoc, linker-signed, teamless or
/// pre-team directory names none, and neither do two that disagree.
#[test]
fn a_signer_is_read_only_from_a_team_signature() {
    let signer = |bodies: &[&[u8]]| {
        CodeDirectories::of_bytes(&test_image(0x0100_000c, bodies))
            .unwrap()
            .signer()
    };
    let team = signed(0x20400, 0, BUNDLE_ID, Some("ABCDE12345"));
    assert_eq!(
        signer(&[&team, &team]),
        Some(Signer {
            identifier: BUNDLE_ID.to_owned(),
            team: "ABCDE12345".to_owned()
        })
    );
    let other = signed(0x20400, 0, BUNDLE_ID, Some("ZZZZZ99999"));
    for bodies in [
        vec![signed(0x20400, CS_ADHOC, BUNDLE_ID, Some("ABCDE12345"))],
        vec![signed(
            0x20400,
            CS_LINKER_SIGNED,
            BUNDLE_ID,
            Some("ABCDE12345"),
        )],
        vec![signed(0x20400, 0, BUNDLE_ID, None)],
        vec![signed(0x20100, 0, BUNDLE_ID, Some("ABCDE12345"))],
        vec![team.clone(), other],
        vec![b"this build".to_vec()],
    ] {
        let bodies: Vec<&[u8]> = bodies.iter().map(Vec::as_slice).collect();
        assert_eq!(signer(&bodies), None, "{bodies:?}");
    }
}

/// Owed while the clone is missing or another build's, or the plist is missing or another
/// source's; current when both match. Another build's image that names the source's own team
/// and identifier is the broker's to adopt, and owes nothing. NEGATIVE CONTROLS: an
/// unreadable clone or plist owes nothing, a source whose own code cannot be read is no
/// reading at all, and an ad-hoc pair, two teams, or another identifier stay owed.
#[test]
fn the_install_is_owed_by_what_an_account_reads() {
    const SOURCE: &str = "/Applications/aterm.app";
    const OURS: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
    let ours = test_image(0x0100_000c, &[b"this build"]);
    let theirs = test_image(0x0100_000c, &[b"another build"]);
    let release = |identifier: &str, team: &str, flags: u32, page: u8| {
        let mut body = signed(0x20400, flags, identifier, Some(team));
        body.push(page);
        test_image(0x0100_000c, &[&body])
    };
    let plist = plist_text(Path::new(SOURCE)).into_bytes();
    let elsewhere = plist_text(Path::new("/Users//a/Applications/aterm.app")).into_bytes();
    let denied = || Err(io::Error::from(io::ErrorKind::PermissionDenied));
    let image = super::super::IMAGE_EXECUTABLE;
    let daemon = super::super::PLIST_PATH;
    type Case<'a> = (Vec<(&'a str, io::Result<Vec<u8>>)>, Part, Part);
    let cases: Vec<Case<'_>> = vec![
        (vec![], Part::Missing, Part::Missing),
        (
            vec![(image, Ok(ours.clone())), (daemon, Ok(plist.clone()))],
            Part::Current,
            Part::Current,
        ),
        (
            vec![(image, Ok(theirs)), (daemon, Ok(plist.clone()))],
            Part::Differs,
            Part::Current,
        ),
        (
            vec![(image, Ok(ours.clone())), (daemon, Ok(elsewhere))],
            Part::Current,
            Part::Differs,
        ),
        (
            vec![(image, denied()), (daemon, denied())],
            Part::Unread(format!("{image}: permission denied")),
            Part::Unread(format!("{daemon}: permission denied")),
        ),
    ];
    for (mut files, image_part, plist_part) in cases {
        files.push((OURS, Ok(ours.clone())));
        check(files, image_part, plist_part);
    }
    let team = "ABCDE12345";
    let dev = "com.aterm.aterm.dev";
    let updates = [
        (
            BUNDLE_ID,
            team,
            0,
            BUNDLE_ID,
            team,
            Part::Adopting(team.to_owned()),
        ),
        (BUNDLE_ID, "ZZZZZ99999", 0, BUNDLE_ID, team, Part::Differs),
        (dev, team, 0, dev, team, Part::Differs),
        (BUNDLE_ID, team, CS_ADHOC, BUNDLE_ID, team, Part::Differs),
    ];
    for (identifier, image_team, flags, source_identifier, source_team, image_part) in updates {
        check(
            vec![
                (image, Ok(release(identifier, image_team, flags, 1))),
                (daemon, Ok(plist.clone())),
                (OURS, Ok(release(source_identifier, source_team, flags, 2))),
            ],
            image_part,
            Part::Current,
        );
    }
    assert!(read(&Fixed::new(vec![]), Path::new(SOURCE)).is_err());
}

/// [`read`] of `files` against the source reads `image` and `plist`, owed and current as
/// those two say.
fn check(files: Vec<(&str, io::Result<Vec<u8>>)>, image: Part, plist: Part) {
    let reading = read(&Fixed::new(files), Path::new("/Applications/aterm.app")).unwrap();
    let owed = image.owed() || plist.owed();
    let current = image == Part::Current && plist == Part::Current;
    assert_eq!(reading, Reading { image, plist });
    assert_eq!(
        (reading.owed(), reading.current()),
        (owed, current),
        "{reading:?}"
    );
}

/// Current files and adoptable signed images need a running service. File repairs do not
/// need a service probe; unknown file or service readings authorize no additional repair.
#[test]
fn a_stopped_service_is_owed_even_when_its_files_need_no_repair() {
    for image in [Part::Current, Part::Adopting("ABCDE12345".into())] {
        let reading = Reading {
            image,
            plist: Part::Current,
        };
        assert!(!reading.owed_with_running(|| Some(true)));
        assert!(reading.owed_with_running(|| Some(false)));
        assert!(!reading.owed_with_running(|| None));
    }
    for reading in [
        Reading {
            image: Part::Missing,
            plist: Part::Current,
        },
        Reading {
            image: Part::Current,
            plist: Part::Differs,
        },
    ] {
        assert!(reading.owed_with_running(|| panic!("file repair needs no service probe")));
    }
    for reading in [
        Reading {
            image: Part::Unread("denied".into()),
            plist: Part::Current,
        },
        Reading {
            image: Part::Current,
            plist: Part::Unread("denied".into()),
        },
    ] {
        assert!(!reading.owed_with_running(|| panic!("unread files authorize no service repair")));
    }
}

/// The source is the bundle around the executable; a bare build has none.
#[test]
fn the_source_is_the_bundle_the_executable_sits_in() {
    assert_eq!(
        source_bundle(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")),
        Some(PathBuf::from("/Applications/aterm.app"))
    );
    assert_eq!(
        source_bundle(Path::new("/Users//a/aterm/target/debug/aterm")),
        None
    );
    assert_eq!(source_bundle(Path::new("/Applications/aterm.app")), None);
}
