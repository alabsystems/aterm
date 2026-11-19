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

/// Every CodeDirectory must bind an `Info.plist` for a number to count, and the slot seals the
/// plist's bytes in the directory's own hash type: the directory the kernel loaded is found by
/// its cdhash. NEGATIVE CONTROLS: a directory with no special slot, one whose slot is zero or
/// whose hash size disagrees with its type binds nothing; another plist's bytes are not sealed;
/// and another cdhash finds no slot.
#[cfg(target_os = "macos")]
#[test]
fn an_info_slot_seals_its_plist_in_every_directory() {
    let info = test_info(120);
    let bound = test_directory(BUNDLE_ID, Some(TEAM), 0, 1, Some(&info));
    let unbound = test_directory(BUNDLE_ID, Some(TEAM), 0, 1, None);
    let code = |bodies: &[&[u8]]| CodeDirectories::of_bytes(&test_image(cpu(), bodies)).unwrap();
    let release = code(&[&bound, &bound]);
    assert!(release.binds_info_plist() && release.seals_info_plist(&info));
    assert!(!release.seals_info_plist(&test_info(121)));
    let blob = &release.0[0].1;
    let cdhash = directory_hash(blob[37], blob).unwrap();
    let slot = release.info_slot(cdhash).unwrap();
    assert!(slot.seals(&info) && !slot.seals(&test_info(999)));
    let mut other = cdhash;
    other[0] ^= 1;
    assert_eq!(release.info_slot(other), None);

    let half = code(&[&bound, &unbound]);
    assert!(!half.binds_info_plist() && !half.seals_info_plist(&info));
    let mut zero = bound.clone();
    let slot_at = usize::try_from(be(&zero, 8)).unwrap() - 8 - 32;
    zero[slot_at..slot_at + 32].fill(0);
    assert!(!code(&[&zero]).binds_info_plist());
    let mut mismatched = bound.clone();
    mismatched[28] = 20;
    assert!(!code(&[&mismatched]).binds_info_plist());
}

struct Fixed {
    files: BTreeMap<PathBuf, io::Result<Vec<u8>>>,
    /// What [`Files::vm_guest`] answers.
    vm_guest: Option<bool>,
}

impl Fixed {
    fn new(entries: Vec<(&str, io::Result<Vec<u8>>)>) -> Self {
        Self {
            files: entries
                .into_iter()
                .map(|(path, bytes)| (PathBuf::from(path), bytes))
                .collect(),
            vm_guest: None,
        }
    }

    fn in_vm(mut self, vm_guest: Option<bool>) -> Self {
        self.vm_guest = vm_guest;
        self
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

    fn vm_guest(&self) -> Option<bool> {
        self.vm_guest
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

const SOURCE: &str = "/Applications/aterm.app";
const OURS: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
const OUR_INFO: &str = "/Applications/aterm.app/Contents/Info.plist";
const TEAM: &str = "ABCDE12345";

/// The image's `Info.plist`.
fn image_info() -> String {
    format!("{}/{BUNDLE_INFO}", super::super::IMAGE_BUNDLE)
}

/// This machine's CPU type, as [`test_release`] writes it.
fn cpu() -> u32 {
    if cfg!(target_arch = "x86_64") {
        0x0100_0007
    } else {
        0x0100_000c
    }
}

/// The reader of build `build`, team signed, its code `page`, and the image of `image` (its code
/// and number) where given, its plist naming `named`.
fn installed(build: u64, page: u8, image: Option<(u64, u8)>, named: &str) -> Fixed {
    let mut files = vec![
        (OURS, Ok(test_release(TEAM, page, &test_info(build)))),
        (OUR_INFO, Ok(test_info(build))),
        (
            super::super::PLIST_PATH,
            Ok(plist_text(Path::new(named)).into_bytes()),
        ),
    ];
    let info = image_info();
    if let Some((image, page)) = image {
        files.push((
            super::super::IMAGE_EXECUTABLE,
            Ok(test_release(TEAM, page, &test_info(image))),
        ));
        files.push((info.as_str(), Ok(test_info(image))));
        return Fixed::new(files);
    }
    Fixed::new(files)
}

/// The build a bundle states is read as the release stamps it: the first `CFBundleVersion` key
/// outside a comment, a nonzero decimal of at most 20 digits that fits a `u64`. NEGATIVE CONTROL
/// (run): a reader that does not skip comments reads the commented-out 999.
#[test]
fn a_bundles_build_is_its_stamped_bundle_version() {
    let stamped = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
                   \t<!-- <key>CFBundleVersion</key><string>999</string> -->\n\
                   \t<key>CFBundleVersion</key>\n\t<!-- the ledger's n -->\n\
                   \t<string>1790120495</string>\n</dict>\n</plist>\n";
    assert_eq!(build_number(stamped.as_bytes()), Some(1_790_120_495));
    assert_eq!(build_number(&test_info(120)), Some(120));
    for value in [
        "0",
        "1.2",
        " 5",
        "123456789012345678901",
        "18446744073709551616",
        "",
    ] {
        let plist = stamped.replace("1790120495", value);
        assert_eq!(build_number(plist.as_bytes()), None, "{value:?}");
    }
    assert_eq!(build_number(b"bplist00\xd1\x01\x02"), None);
    assert_eq!(
        build_number(b"<key>CFBundleVersion</key><!-- never closed"),
        None
    );
    assert_eq!(
        build_number(b"<key>CFBundleShortVersionString</key><string>5</string>"),
        None
    );
    assert_eq!(
        build_number(b"<key>CFBundleVersion</key><integer>5</integer>"),
        None
    );
}

/// Owed while the clone is missing or another signer's, or the plist is missing or one this
/// build writes for no bundle; current when the clone is this build's code and the plist this
/// build's for any bundle. NEGATIVE CONTROLS: an unreadable clone or plist owes nothing, a
/// source whose own code cannot be read is no reading at all, and in a VM guest an ad-hoc pair or
/// another identifier stays owed.
#[test]
fn the_install_is_owed_by_what_an_account_reads() {
    let ours = test_release(TEAM, 1, &test_info(120));
    let theirs = test_release("ZZZZZ99999", 2, &test_info(130));
    let plist = plist_text(Path::new(SOURCE)).into_bytes();
    let elsewhere = plist_text(Path::new("/Users//a/Applications/aterm.app")).into_bytes();
    let edited = String::from_utf8(plist.clone())
        .unwrap()
        .replace("KeepAlive", "keepAlive")
        .into_bytes();
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
            Part::Current,
        ),
        (
            vec![(image, Ok(ours.clone())), (daemon, Ok(edited))],
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
        files.push((OUR_INFO, Ok(test_info(120))));
        check(Fixed::new(files), image_part, plist_part);
    }
    // In a VM guest a development build reads as a release does: unsigned or ad hoc, or of
    // another identifier, another build's image is owed.
    let unsigned = |identifier: &str, team: Option<&str>, flags: u32, page: u8| {
        test_image(
            cpu(),
            &[&test_directory(
                identifier,
                team,
                flags,
                page,
                Some(&test_info(120)),
            )],
        )
    };
    let dev = "com.aterm.aterm.dev";
    for (identifier, team, flags) in [
        (BUNDLE_ID, Some(TEAM), CS_ADHOC),
        (BUNDLE_ID, None, 0),
        (dev, Some(TEAM), 0),
    ] {
        let files = vec![
            (image, Ok(unsigned(identifier, team, flags, 1))),
            (daemon, Ok(plist.clone())),
            (OURS, Ok(unsigned(identifier, team, flags, 2))),
            (OUR_INFO, Ok(test_info(120))),
        ];
        check(
            Fixed::new(files).in_vm(Some(true)),
            Part::Differs,
            Part::Current,
        );
    }
    assert!(read(&Fixed::new(vec![]), Path::new(SOURCE)).is_err());
}

/// A bundle whose executable states no team signer of aterm's (`ship dev-app`'s: ad hoc,
/// self-signed with no team, or Developer ID under `com.aterm.aterm.dev`) owns no broker outside
/// a VM guest: whatever is installed, both parts read Development, owed by nothing whether the
/// job runs or is listed stopped, and an unread VM reading counts as a real Mac. In a VM guest
/// it reads as today, so a release's image is another build's and owed. NEGATIVE CONTROL (run):
/// without the Development return the real Mac's reading is Differs and owed.
#[test]
fn a_development_build_owes_no_broker_outside_a_vm_guest() {
    let release = test_release(TEAM, 9, &test_info(130));
    let plist = plist_text(Path::new(SOURCE)).into_bytes();
    for (what, directory) in [
        (
            "ad hoc",
            test_directory(BUNDLE_ID, None, CS_ADHOC, 1, Some(&test_info(120))),
        ),
        (
            "self-signed, no team",
            test_directory(BUNDLE_ID, None, 0, 1, Some(&test_info(120))),
        ),
        (
            "Developer ID, another identifier",
            test_directory("com.aterm.aterm.dev", Some(TEAM), 0, 1, None),
        ),
    ] {
        let reader = test_image(cpu(), &[&directory]);
        let files = || {
            vec![
                (OURS, Ok(reader.clone())),
                (OUR_INFO, Ok(test_info(120))),
                (super::super::IMAGE_EXECUTABLE, Ok(release.clone())),
                (super::super::PLIST_PATH, Ok(plist.clone())),
            ]
        };
        for vm_guest in [None, Some(false)] {
            let reading = read(&Fixed::new(files()).in_vm(vm_guest), Path::new(SOURCE)).unwrap();
            assert_eq!(
                reading,
                Reading {
                    image: Part::Development,
                    plist: Part::Development
                },
                "{what} {vm_guest:?}"
            );
            assert!(!reading.owed() && reading.in_place(), "{what}");
            for job in [
                Some(JobState::Running),
                Some(JobState::Idle),
                Some(JobState::Unloaded),
                None,
            ] {
                assert!(
                    !reading.owed_with_job(|| job, || Some(false)),
                    "{what} job {job:?}"
                );
            }
        }
        let in_vm = read(&Fixed::new(files()).in_vm(Some(true)), Path::new(SOURCE)).unwrap();
        assert_eq!(in_vm.image, Part::Differs, "{what}");
        assert!(in_vm.owed() && !in_vm.in_place(), "{what}");
    }
}

/// The plist is in place whichever bundle it names: a job whose source is a cut's bundle since
/// deleted, or an older copy at 100, pins nothing, and the reader at 120 reads its signer's
/// older image adopting, which its ask then brings about. NEGATIVE CONTROL (run): a plist read
/// against this build's own bundle alone reads Differs, owed, as the held reading did.
#[test]
fn an_abandoned_or_older_holder_does_not_pin_the_reading() {
    const OLD: &str = "/Users//alice/old/aterm.app";
    for (named, holder) in [("/Users//alice/gone/aterm.app", false), (OLD, true)] {
        let mut files = installed(120, 2, Some((100, 1)), named);
        if holder {
            for (path, bytes) in [
                (
                    format!("{OLD}/{BUNDLE_EXECUTABLE}"),
                    test_release(TEAM, 1, &test_info(100)),
                ),
                (format!("{OLD}/{BUNDLE_INFO}"), test_info(100)),
            ] {
                files.files.insert(PathBuf::from(path), Ok(bytes));
            }
        }
        let reading = read(&files, Path::new(SOURCE)).unwrap();
        assert_eq!(
            reading,
            Reading {
                image: Part::Adopting(TEAM.to_owned()),
                plist: Part::Current
            },
            "{named}"
        );
        assert!(!reading.owed(), "{named}");
    }
}

/// A kept image ([`Part::Kept`]) owes this older build nothing, whatever launchd holds of its
/// job: running, held with no pid (stopped, or between two of launchd's starts), held not at all
/// (which may be inside the newer build's own `bootout` and `bootstrap`), or unsaid.
fn kept_never_owed(reading: &Reading) {
    for job in [
        Some(JobState::Running),
        Some(JobState::Idle),
        Some(JobState::Unloaded),
        None,
    ] {
        assert!(
            !reading.owed_with_job(|| job, || panic!("nothing owed asks no switch")),
            "{reading:?} {job:?}"
        );
    }
}

/// A kept image's plist is the newer build's, whatever it says: missing, another build's or one
/// only the newer build writes, it owes this older build nothing and reads in place, so the
/// probe that keeps the image and the reading that would ask agree, and no Mac password is asked
/// at every launch for an elevation that does nothing, whatever launchd holds of its job.
/// NEGATIVE CONTROLS (run): with `owed` counting a kept image's plist, the reading owes it; with
/// a kept image owed where launchd holds no job, a newer build's own restart of it asks this
/// account's password.
#[test]
fn a_kept_images_plist_is_the_newer_builds() {
    for plist in [Part::Missing, Part::Differs, Part::Unread("denied".into())] {
        let reading = Reading {
            image: Part::Kept {
                team: TEAM.to_owned(),
                image: 120,
                ours: 100,
            },
            plist,
        };
        assert!(!reading.owed() && reading.in_place(), "{reading:?}");
        kept_never_owed(&reading);
    }
}

/// An older build beside a newer image of its own signer reads it kept, owed by nothing whatever
/// launchd holds of its job; so do equal numbers of other code. An image
/// whose number does not read is adopting while this build's reads; this build's own number
/// unread is no judgment. NEGATIVE CONTROL (run): without the comparison, the older reader
/// reads the newer image adopting.
#[test]
fn an_older_build_reads_a_newer_image_kept() {
    let kept = |image, ours| Part::Kept {
        team: TEAM.to_owned(),
        image,
        ours,
    };
    for (build, image, part) in [(100, 120, kept(120, 100)), (120, 120, kept(120, 120))] {
        let reading = read(
            &installed(build, 2, Some((image, 1)), SOURCE),
            Path::new(SOURCE),
        )
        .unwrap();
        assert_eq!(
            reading,
            Reading {
                image: part,
                plist: Part::Current
            }
        );
        assert!(!reading.owed() && reading.in_place() && !reading.current());
        kept_never_owed(&reading);
    }
    let mut unnumbered = installed(120, 2, Some((130, 1)), SOURCE);
    unnumbered.files.remove(Path::new(&image_info()));
    assert_eq!(
        read(&unnumbered, Path::new(SOURCE)).unwrap().image,
        Part::Adopting(TEAM.to_owned())
    );
    let mut ours_unnumbered = installed(120, 2, Some((100, 1)), SOURCE);
    ours_unnumbered.files.remove(Path::new(OUR_INFO));
    let reading = read(&ours_unnumbered, Path::new(SOURCE)).unwrap();
    assert!(
        matches!(&reading.image, Part::Unread(why) if why.contains(OUR_INFO)),
        "{reading:?}"
    );
    assert!(!reading.owed());
}

/// A process asks the broker to consider its bundle only where its image reads adopting, from a
/// bundle, outside a test binary and a scratch world: one ask on its way at a time, kept only
/// once a broker heard it, and made again of another broker (one that restarted heard none) and,
/// while the image still reads adopting, of the same broker after [`ASK_AGAIN`]: that broker's
/// bounded list may have let the ask go. An ask no broker heard (the broker restarting, at its
/// connection bound, or turning the process away unchecked) is made again at the next reading.
/// NEGATIVE CONTROLS (run): without the scratch-world guard a world's qualification run asks;
/// with an ask kept as heard when it starts, a lost ask is never made again; with a heard ask
/// kept for good, an ask the broker let go is never made again.
#[test]
fn a_process_asks_until_a_broker_hears_it_and_again_of_a_new_broker() {
    let adopting = Reading {
        image: Part::Adopting(TEAM.to_owned()),
        plist: Part::Current,
    };
    fn asking(reading: &Reading) -> Asking<'_> {
        Asking {
            test_binary: false,
            world: false,
            bundled: true,
            reading,
        }
    }
    let (one, two) = (Some((1, 7)), Some((1, 8)));
    let at = Instant::now();
    let ask = Ask::new();
    for (why, gate) in [
        (
            "a scratch world",
            Asking {
                world: true,
                ..asking(&adopting)
            },
        ),
        (
            "a test binary",
            Asking {
                test_binary: true,
                ..asking(&adopting)
            },
        ),
        (
            "outside a bundle",
            Asking {
                bundled: false,
                ..asking(&adopting)
            },
        ),
    ] {
        assert!(!ask.begin(&gate, one, at), "{why}");
    }
    for image in [
        Part::Kept {
            team: TEAM.to_owned(),
            image: 120,
            ours: 100,
        },
        Part::Current,
        Part::Development,
        Part::Differs,
    ] {
        let reading = Reading {
            image,
            plist: Part::Current,
        };
        assert!(!ask.begin(&asking(&reading), one, at), "{reading:?}");
    }
    assert!(ask.begin(&asking(&adopting), one, at));
    assert!(
        !ask.begin(&asking(&adopting), one, at),
        "one ask on its way at a time"
    );
    ask.end(None, at);
    assert!(
        ask.begin(&asking(&adopting), one, at),
        "an ask no broker heard is made again"
    );
    ask.end(one, at);
    assert!(
        !ask.begin(
            &asking(&adopting),
            one,
            at + ASK_AGAIN - Duration::from_secs(1)
        ),
        "the broker that heard it is not asked again within {ASK_AGAIN:?}"
    );
    assert!(
        ask.begin(&asking(&adopting), one, at + ASK_AGAIN),
        "the broker that heard it is asked again after {ASK_AGAIN:?}, while the image reads \
         adopting"
    );
    ask.end(one, at + ASK_AGAIN);
    assert!(
        !ask.begin(&asking(&adopting), one, at + ASK_AGAIN),
        "the ask made again counts from when that broker heard it"
    );
    assert!(
        ask.begin(&asking(&adopting), two, at),
        "a broker that restarted is asked again"
    );
    ask.end(None, at);
    assert!(
        ask.begin(&asking(&adopting), None, at),
        "an unread socket is no broker that heard it"
    );
}

/// One ask is tried until a broker hears it, a look apart and doubling, and names the broker
/// that heard it; it ends early once this process no longer asks, and after [`ASK_TRIES`] tries
/// names none. NEGATIVE CONTROL (run): an ask tried once is lost to a broker that was restarting.
#[test]
fn an_ask_is_tried_until_a_broker_hears_it() {
    let looked = std::cell::Cell::new(0_u64);
    let generation = || {
        looked.set(looked.get() + 1);
        Some((1, looked.get()))
    };
    let waits = std::cell::RefCell::new(Vec::new());
    let tries = std::cell::Cell::new(0_u32);
    let heard = ask_until_heard(
        || {
            tries.set(tries.get() + 1);
            tries.get() == 3
        },
        generation,
        |wait| waits.borrow_mut().push(wait),
        || true,
    );
    assert_eq!(
        heard,
        Some((1, 3)),
        "the broker each try looked at, before it"
    );
    assert_eq!(
        *waits.borrow(),
        [ask_again_after(1), ask_again_after(2)],
        "a look, then two"
    );
    assert_eq!(ask_again_after(1), super::super::ADOPTION_LOOK);
    assert_eq!(ask_again_after(20), Duration::from_secs(5 * 60));

    tries.set(0);
    waits.borrow_mut().clear();
    assert_eq!(
        ask_until_heard(
            || {
                tries.set(tries.get() + 1);
                false
            },
            generation,
            |wait| waits.borrow_mut().push(wait),
            || false
        ),
        None,
        "a process that no longer asks stops"
    );
    assert_eq!((tries.get(), waits.borrow().len()), (1, 1));

    tries.set(0);
    assert_eq!(
        ask_until_heard(
            || {
                tries.set(tries.get() + 1);
                false
            },
            generation,
            |_| {},
            || true
        ),
        None
    );
    assert_eq!(tries.get(), ASK_TRIES);
}

/// A development build is told by the code the kernel loaded for it, read at the file its
/// executable is (every link on the way followed): a team release reached through a link (an app
/// bundle's tools, `~/.local/bin/aterm`) is no development build, an ad hoc build reached through
/// one is, an executable whose signature does not read is none, and so is a file that no longer
/// holds the loaded code: a link retargeted to a release after an ad hoc build started (or to an
/// ad hoc build after a release did) changes nothing, nor does an unread cdhash. NEGATIVE
/// CONTROLS (run): without following the link the ad hoc build reads as a release; with an unread
/// signature counted as development the unreadable one is; without matching the loaded cdhash a
/// release started before its link was retargeted to an ad hoc build reads as development.
#[cfg(target_os = "macos")]
#[test]
fn a_development_build_is_told_by_the_code_it_loaded() {
    let root = aterm_tempfile::tempdir().unwrap();
    let at = |name: &str, bytes: &[u8]| {
        let file = root.path().join(name);
        std::fs::write(&file, bytes).unwrap();
        let link = root.path().join(format!("{name}-link"));
        std::os::unix::fs::symlink(&file, &link).unwrap();
        (file, link)
    };
    let release_bytes = test_release(TEAM, 1, &test_info(130));
    let released = test_cdhash(&release_bytes);
    let (release, release_link) = at("release", &release_bytes);
    let ad_hoc_bytes = test_image(
        cpu(),
        &[&test_directory(BUNDLE_ID, None, CS_ADHOC, 1, None)],
    );
    let built = test_cdhash(&ad_hoc_bytes);
    assert!(released.is_some() && built.is_some() && released != built);
    let (ad_hoc, ad_hoc_link) = at("ad-hoc", &ad_hoc_bytes);
    let (unread, unread_link) = at("unread", b"#!/bin/sh\n");
    for (exe, loaded, development) in [
        (&release, released, false),
        (&release_link, released, false),
        (&ad_hoc, built, true),
        (&ad_hoc_link, built, true),
        (&unread, built, false),
        (&unread_link, built, false),
        (&root.path().join("absent"), built, false),
        (&ad_hoc, None, false),
        (&ad_hoc_link, released, false),
        (&release_link, built, false),
    ] {
        assert_eq!(
            development_at(exe, loaded),
            development,
            "{} loading {loaded:02x?}",
            exe.display()
        );
    }
}

/// The bundle a plist names is read back only from a plist this build writes for it, whatever
/// its path's characters.
#[test]
fn a_plists_source_is_read_back_exactly() {
    for source in [
        "/Applications/aterm.app",
        "/Users//a & b/<new> \"cut\" 'app'/aterm.app",
        "/Users//a/&amp;lt;/aterm.app",
        "/Users//a/&lt;/aterm.app",
    ] {
        let plist = plist_text(Path::new(source));
        assert_eq!(
            named_source(plist.as_bytes()),
            Some(PathBuf::from(source)),
            "{plist}"
        );
    }
    let relative = plist_text(Path::new("Applications/aterm.app"));
    assert_eq!(named_source(relative.as_bytes()), None);
    let edited = plist_text(Path::new("/Applications/aterm.app")).replace("KeepAlive", "keepAlive");
    assert_eq!(named_source(edited.as_bytes()), None);
    assert_eq!(named_source(b"\xff"), None);
}

/// [`read`] of `files` against the source reads `image` and `plist`, owed and current as
/// those two say.
fn check(files: Fixed, image: Part, plist: Part) {
    let reading = read(&files, Path::new(SOURCE)).unwrap();
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
/// need a service probe; unknown file or service readings authorize no additional repair. A job
/// launchd cannot say is switched off keeps the debt as it reads.
#[test]
fn a_stopped_service_is_owed_even_when_its_files_need_no_repair() {
    for image in [Part::Current, Part::Adopting("ABCDE12345".into())] {
        let reading = Reading {
            image,
            plist: Part::Current,
        };
        assert!(!reading.owed_with_job(
            || Some(JobState::Running),
            || panic!("nothing owed asks no switch")
        ));
        for stopped in [JobState::Idle, JobState::Unloaded] {
            assert!(reading.owed_with_job(|| Some(stopped), || Some(false)));
            assert!(reading.owed_with_job(|| Some(stopped), || None));
        }
        assert!(!reading.owed_with_job(|| None, || panic!("nothing owed asks no switch")));
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
        assert!(reading.owed_with_job(
            || panic!("file repair needs no service probe"),
            || Some(false)
        ));
        assert!(reading.owed_with_job(|| panic!("file repair needs no service probe"), || None));
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
        assert!(!reading.owed_with_job(
            || panic!("unread files authorize no service repair"),
            || panic!("nothing owed asks no switch")
        ));
    }
}

/// A broker job a person switched off (its background item in Login Items, or `launchctl
/// disable`) is owed nothing, stopped or running, its files current, adoptable, missing or
/// another build's: no password is asked to undo their choice.
/// MUTANT: `owed_with_job` ignoring `switched_off` owes every one of them.
#[test]
fn a_job_the_person_switched_off_is_owed_nothing() {
    let parts = [
        (Part::Current, Part::Current),
        (Part::Adopting("ABCDE12345".into()), Part::Current),
        (Part::Missing, Part::Missing),
        (Part::Differs, Part::Current),
        (Part::Current, Part::Differs),
    ];
    for (image, plist) in parts {
        let reading = Reading { image, plist };
        for job in [
            Some(JobState::Running),
            Some(JobState::Idle),
            Some(JobState::Unloaded),
            None,
        ] {
            assert!(
                !reading.owed_with_job(|| job, || Some(true)),
                "{reading:?} job {job:?}"
            );
        }
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

/// The broker serves this build's requests only from this build's own install: the plist and
/// the image this build's, and launchd running it. An image still to be adopted runs an earlier
/// build's broker, which may predate the request, and one the broker keeps over this build a
/// later build's, whose requests this build cannot know; and `job` is read only where the files
/// serve.
/// NEGATIVE CONTROL: accepting an image still to be adopted, or one the broker keeps, counts
/// another build's broker as serving this one.
#[test]
fn only_this_builds_own_running_install_serves_its_requests() {
    let ours = Reading {
        image: Part::Current,
        plist: Part::Current,
    };
    assert!(ours.serves_with(|| Some(JobState::Running)));
    assert!(!ours.serves_with(|| Some(JobState::Idle)));
    assert!(!ours.serves_with(|| Some(JobState::Unloaded)));
    assert!(!ours.serves_with(|| None));
    let adopting = Reading {
        image: Part::Adopting("ABCDE12345".into()),
        ..ours.clone()
    };
    let kept = Reading {
        image: Part::Kept {
            team: "ABCDE12345".into(),
            image: 9,
            ours: 8,
        },
        ..ours.clone()
    };
    let differs = Reading {
        plist: Part::Differs,
        ..ours.clone()
    };
    for reading in [adopting, kept, differs] {
        assert!(
            !reading.serves_with(|| panic!("{reading:?} reads no job")),
            "{reading:?}"
        );
    }
}
