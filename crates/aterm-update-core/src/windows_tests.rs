// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::Cursor;

/// Where the synthetic images' PE signature sits (MSVC's link.exe puts it near here).
const E_LFANEW: usize = 0x80;

/// The start of a PE32+ image for `machine` declaring `subsystem`, as a linker lays it out:
/// MZ header, `PE\0\0`, COFF header (three sections, an executable large-address-aware image),
/// and a 240-byte optional header, then zero padding.
fn image(machine: u16, subsystem: u16) -> Vec<u8> {
    let mut bytes = vec![0; 1024];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&(E_LFANEW as u32).to_le_bytes());
    let pe = E_LFANEW;
    bytes[pe..pe + 4].copy_from_slice(b"PE\0\0");
    bytes[pe + 4..pe + 6].copy_from_slice(&machine.to_le_bytes());
    bytes[pe + 6..pe + 8].copy_from_slice(&3_u16.to_le_bytes());
    bytes[pe + 20..pe + 22].copy_from_slice(&240_u16.to_le_bytes());
    bytes[pe + 22..pe + 24].copy_from_slice(&0x0022_u16.to_le_bytes());
    let optional = pe + 24;
    bytes[optional..optional + 2].copy_from_slice(&0x20b_u16.to_le_bytes());
    bytes[optional + 68..optional + 70].copy_from_slice(&subsystem.to_le_bytes());
    bytes
}

fn console() -> Vec<u8> {
    image(0x8664, Subsystem::Console.code())
}

fn windowed() -> Vec<u8> {
    image(0x8664, Subsystem::Windowed.code())
}

#[test]
fn canonical_names_do_not_alias_other_targets_or_platforms() {
    let target = WindowsTarget::X86_64;
    assert_eq!(target.triple(), "x86_64-pc-windows-msvc");
    assert_eq!(target.arch(), "x86_64");
    assert_eq!(
        target.asset_name("0.200.0"),
        "aterm-0.200.0-windows-x86_64.zip"
    );
    assert_ne!(
        target.asset_name("0.200.0"),
        crate::linux::LinuxTarget::X86_64.asset_name("0.200.0")
    );
    assert_eq!(WindowsTarget::ALL, [WindowsTarget::X86_64]);
    assert_eq!(
        WindowsTarget::native(),
        cfg!(all(target_os = "windows", target_arch = "x86_64")).then_some(target)
    );
}

#[test]
fn each_image_passes_only_as_its_own_subsystem() {
    let target = WindowsTarget::X86_64;
    target
        .validate_pe_header(&console(), Subsystem::Console)
        .unwrap();
    target
        .validate_pe_header(&windowed(), Subsystem::Windowed)
        .unwrap();
    let swapped = target
        .validate_pe_header(&console(), Subsystem::Windowed)
        .unwrap_err();
    assert!(swapped.contains("subsystem 3"), "{swapped}");
    assert!(
        target
            .validate_pe_header(&windowed(), Subsystem::Console)
            .is_err()
    );
}

#[test]
fn pe_compatibility_is_fail_closed() {
    let target = WindowsTarget::X86_64;
    let good = console();
    // Every prefix short of the optional header's fixed part.
    for length in 0..E_LFANEW + 24 + 112 {
        assert!(
            target
                .validate_pe_header(&good[..length], Subsystem::Console)
                .is_err(),
            "length {length}"
        );
    }
    target
        .validate_pe_header(&good[..E_LFANEW + 24 + 112], Subsystem::Console)
        .unwrap();
    let pe = E_LFANEW;
    let optional = pe + 24;
    let mutants: [(&str, usize, &[u8]); 13] = [
        ("MZ magic", 0, b"ZM"),
        ("e_lfanew inside the DOS header", 60, &32_u32.to_le_bytes()),
        ("e_lfanew past the prefix", 60, &0x1000_u32.to_le_bytes()),
        ("e_lfanew overflowing", 60, &u32::MAX.to_le_bytes()),
        ("PE signature", pe, b"PE\0\x01"),
        ("i386 machine", pe + 4, &0x014c_u16.to_le_bytes()),
        ("arm64 machine", pe + 4, &0xaa64_u16.to_le_bytes()),
        ("no section", pe + 6, &0_u16.to_le_bytes()),
        ("short optional header", pe + 20, &96_u16.to_le_bytes()),
        (
            "not an executable image",
            pe + 22,
            &0x0020_u16.to_le_bytes(),
        ),
        ("a DLL", pe + 22, &0x2022_u16.to_le_bytes()),
        ("PE32, not PE32+", optional, &0x10b_u16.to_le_bytes()),
        ("native subsystem", optional + 68, &1_u16.to_le_bytes()),
    ];
    for (what, at, patch) in mutants {
        let mut bad = good.clone();
        bad[at..at + patch.len()].copy_from_slice(patch);
        assert!(
            target.validate_pe_header(&bad, Subsystem::Console).is_err(),
            "{what}"
        );
    }
}

/// The test binary is a console image the real MSVC linker wrote: its own header passes as
/// the console image of its target and as nothing else.
#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn a_linker_written_console_image_passes_as_the_console_image() {
    let exe = std::env::current_exe().unwrap();
    let mut head = Vec::new();
    std::fs::File::open(&exe)
        .unwrap()
        .take(PE_HEADER_PREFIX)
        .read_to_end(&mut head)
        .unwrap();
    let target = WindowsTarget::X86_64;
    target
        .validate_pe_header(&head, Subsystem::Console)
        .unwrap();
    assert!(
        target
            .validate_pe_header(&head, Subsystem::Windowed)
            .is_err()
    );
}

/// One member of a synthetic zip.
struct Member {
    name: Vec<u8>,
    data: Vec<u8>,
    deflate: bool,
    flags: u16,
    method: Option<u16>,
    /// Unix mode bits in the external attributes (host byte 3); 0 writes a DOS host record.
    mode: u32,
}

fn member(name: &str, data: &[u8]) -> Member {
    Member {
        name: name.as_bytes().to_vec(),
        data: data.to_vec(),
        deflate: false,
        flags: 0,
        method: None,
        mode: 0,
    }
}

fn deflated(name: &str, data: &[u8]) -> Member {
    Member {
        deflate: true,
        ..member(name, data)
    }
}

/// `data` as raw DEFLATE: stored blocks (BTYPE 00), the last one final.
fn stored_deflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunks = data.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[0x01, 0, 0, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let n = chunk.len() as u16;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(!n).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// A complete archive of `members` (no ZIP64, no comment).
fn zip(members: &[Member]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for m in members {
        let body = if m.deflate {
            stored_deflate(&m.data)
        } else {
            m.data.clone()
        };
        let method = m.method.unwrap_or(if m.deflate { 8 } else { 0 });
        let offset = out.len() as u32;
        let fixed = |into: &mut Vec<u8>| {
            into.extend_from_slice(&20_u16.to_le_bytes());
            into.extend_from_slice(&m.flags.to_le_bytes());
            into.extend_from_slice(&method.to_le_bytes());
            into.extend_from_slice(&[0; 8]);
            into.extend_from_slice(&(body.len() as u32).to_le_bytes());
            into.extend_from_slice(&(m.data.len() as u32).to_le_bytes());
            into.extend_from_slice(&(m.name.len() as u16).to_le_bytes());
            into.extend_from_slice(&0_u16.to_le_bytes());
        };
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        fixed(&mut out);
        out.extend_from_slice(&m.name);
        out.extend_from_slice(&body);
        let host: u16 = if m.mode == 0 { 0 } else { 3 };
        central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        central.extend_from_slice(&((host << 8) | 20).to_le_bytes());
        fixed(&mut central);
        central.extend_from_slice(&[0; 6]);
        central.extend_from_slice(&(m.mode << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(&m.name);
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

/// The portable folder as `Compress-Archive -Path dist\aterm` writes it: every member under
/// `aterm/`, the images deflated, a nested licence folder.
fn portable() -> Vec<Member> {
    let mut console_image = console();
    console_image.extend(std::iter::repeat_n(0x90, 70_000));
    vec![
        member("aterm/", b""),
        deflated(CONSOLE_IMAGE, &console_image),
        deflated(WINDOWED_IMAGE, &windowed()),
        deflated("aterm/atpkg.exe", &console()),
        member("aterm/OpenConsole.exe", &image(0x8664, 3)),
        deflated("aterm/README.txt", b"aterm - standalone Windows terminal"),
        member("aterm/licenses/", b""),
        deflated("aterm/licenses/MIT.txt", b"MIT"),
        member("aterm/console.txt", b"not a device"),
        member("aterm/com10.txt", b"not a device"),
    ]
}

fn check(members: &[Member]) -> Result<(), String> {
    WindowsTarget::X86_64.validate_zip(Cursor::new(zip(members)))
}

#[test]
fn the_portable_folder_zip_passes() {
    check(&portable()).unwrap();
    // Folder records are optional: a zip of files alone is the same folder.
    let files: Vec<Member> = portable()
        .into_iter()
        .filter(|m| !m.name.ends_with(b"/"))
        .collect();
    check(&files).unwrap();
}

#[test]
fn a_zip_missing_an_image_is_refused() {
    for missing in [CONSOLE_IMAGE, WINDOWED_IMAGE] {
        let members: Vec<Member> = portable()
            .into_iter()
            .filter(|m| m.name != missing.as_bytes())
            .collect();
        let why = check(&members).unwrap_err();
        assert!(why.contains(&format!("has no {missing}")), "{why}");
    }
}

#[test]
fn an_image_that_is_not_its_pe_is_refused() {
    let cases: [(&str, Vec<u8>); 5] = [
        (WINDOWED_IMAGE, b"#!/bin/sh\necho not a PE image\n".to_vec()),
        (WINDOWED_IMAGE, Vec::new()),
        (WINDOWED_IMAGE, console()),
        (CONSOLE_IMAGE, windowed()),
        (CONSOLE_IMAGE, image(0xaa64, 3)),
    ];
    for (name, data) in cases {
        let mut members = portable();
        for m in &mut members {
            if m.name == name.as_bytes() {
                m.data.clone_from(&data);
            }
        }
        let why = check(&members).unwrap_err();
        assert!(why.starts_with(&format!("{name}: ")), "{why}");
    }
}

#[test]
fn bytes_that_are_not_a_zip_are_refused() {
    let target = WindowsTarget::X86_64;
    let whole = zip(&portable());
    for bytes in [
        Vec::new(),
        b"PK\x03\x04".to_vec(),
        console(),
        whole[..whole.len() - 1].to_vec(),
        [whole.clone(), b"trailing".to_vec()].concat(),
    ] {
        let why = target.validate_zip(Cursor::new(bytes)).unwrap_err();
        assert!(
            why.starts_with("Windows artifact is not a readable zip"),
            "{why}"
        );
    }
}

#[test]
fn members_an_extractor_could_place_elsewhere_are_refused() {
    for name in [
        "aterm.exe",
        "other/aterm.exe",
        "/aterm/x",
        "aterm\\aterm.exe",
        "aterm/../x",
        "aterm/./x",
        "aterm//x",
        "aterm/a\\b",
        "aterm/C:x",
        "aterm/a?b",
        "aterm/a\u{1}b",
        "aterm/x.",
        "aterm/x ",
        "aterm/dir./x",
        "aterm/CON",
        "aterm/nul.txt",
        "aterm/Com1",
        "aterm/lpt9.log",
        "aterm/COM\u{b9}",
    ] {
        let mut members = portable();
        members.push(member(name, b"x"));
        let why = check(&members).unwrap_err();
        assert!(why.contains("member"), "{name}: {why}");
    }
    let mut members = portable();
    members.push(Member {
        name: b"aterm/\xff".to_vec(),
        ..member("", b"x")
    });
    assert!(check(&members).unwrap_err().contains("not UTF-8"));
}

#[test]
fn aliases_and_files_used_as_folders_are_refused() {
    for (extra, why) in [
        (["aterm/readme.TXT", ""], "twice"),
        (["aterm/ATERM.EXE", ""], "twice"),
        (["aterm/README.txt/", ""], "twice"),
        (["aterm/x", "aterm/x/"], "twice"),
        (["aterm/x", "aterm/x/y"], "another member is inside"),
        (["aterm/x", "aterm/X/y"], "another member is inside"),
        (["aterm/licenses/MIT.txt/z", ""], "another member is inside"),
    ] {
        let mut members = portable();
        for name in extra.iter().filter(|name| !name.is_empty()) {
            members.push(member(name, b""));
        }
        let refused = check(&members).unwrap_err();
        assert!(refused.contains(why), "{extra:?}: {refused}");
    }
}

#[test]
fn encrypted_unknown_method_and_symlink_members_are_refused() {
    type Bend = fn(&mut Member);
    let mutants: [(&str, Bend, &str); 4] = [
        ("encrypted", |m| m.flags = 0x0001, "encrypted"),
        ("strong encryption", |m| m.flags = 0x0040, "encrypted"),
        (
            "bzip2",
            |m| m.method = Some(12),
            "neither stored nor deflated",
        ),
        ("symlink", |m| m.mode = 0o120_777, "symbolic link"),
    ];
    for (what, bend, why) in mutants {
        let mut members = portable();
        let readme = members
            .iter_mut()
            .find(|m| m.name == b"aterm/README.txt")
            .unwrap();
        bend(readme);
        let refused = check(&members).unwrap_err();
        assert!(refused.contains(why), "{what}: {refused}");
    }
    // A regular file's Unix mode is no refusal.
    let mut members = portable();
    members[5].mode = 0o100_644;
    check(&members).unwrap();
}

#[test]
fn a_zip_declaring_more_members_than_the_bound_is_refused() {
    let mut members = portable();
    let names: Vec<String> = (members.len() as u64..=MAX_ZIP_ENTRIES)
        .map(|n| format!("aterm/f{n}"))
        .collect();
    members.extend(names.iter().map(|name| member(name, b"")));
    assert_eq!(members.len() as u64, MAX_ZIP_ENTRIES + 1);
    let why = check(&members).unwrap_err();
    assert!(why.contains("more than 4096"), "{why}");
    members.pop();
    check(&members).unwrap();
}
