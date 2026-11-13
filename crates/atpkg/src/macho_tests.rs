// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::fixtures::{
    LOAD_DYLIB, archive, dylib_command, dylib_command32, fat, other_command, rpath_command, thin,
    thin32,
};
use super::*;

fn tmp(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-macho-{label}-"))
}

fn with_tail(head: &[u8]) -> Vec<u8> {
    let mut bytes = head.to_vec();
    bytes.extend_from_slice(&[0u8; 64]);
    bytes
}

/// `magic` (big-endian) then `nfat` in the given byte order.
fn fat_header(magic: u32, nfat: u32, big_endian: bool) -> Vec<u8> {
    let mut h = magic.to_be_bytes().to_vec();
    if big_endian {
        h.extend_from_slice(&nfat.to_be_bytes());
    } else {
        h.extend_from_slice(&nfat.to_le_bytes());
    }
    h
}

#[test]
fn thin_and_fat_headers_are_recognised_at_any_arch_count() {
    for magic in THIN_MAGICS {
        assert!(is_macho_header(&magic.to_be_bytes()), "{magic:#x}");
    }
    for magic in FAT_MAGICS {
        for nfat in [1u32, 2, 44, 45, 52, 200, 818, u32::MAX] {
            for big_endian in [true, false] {
                assert!(
                    is_macho_header(&fat_header(magic, nfat, big_endian)),
                    "{magic:#x} nfat={nfat} big_endian={big_endian}"
                );
            }
        }
    }
    let java8 = [0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x00, 0x00, 0x34];
    assert!(
        is_macho_header(&java8),
        "a class file is checked, not trusted"
    );
}

#[test]
fn empty_fat_headers_and_other_files_are_not() {
    for magic in FAT_MAGICS {
        assert!(!is_macho_header(&fat_header(magic, 0, true)), "{magic:#x}");
    }
    assert!(!is_macho_header(&[0xCA, 0xFE, 0xBA, 0xBE]));
    assert!(!is_macho_header(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 1]));
    assert!(!is_macho_header(&[0xFE, 0xED]));
    assert!(!is_macho_header(b"\x7fELF\x02\x01\x01\x00"));
    assert!(!is_macho_header(b"#!/bin/sh\n"));
    assert!(!is_macho_header(b""));
}

#[test]
fn the_walk_finds_thin_and_fat_files_by_content() {
    let d = tmp("walk");
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::create_dir_all(d.join("libexec/deep")).unwrap();
    std::fs::write(d.join("bin/tool"), with_tail(&0xCFFA_EDFEu32.to_be_bytes())).unwrap();
    let fat = fat_header(0xCAFE_BABE, 2, true);
    std::fs::write(d.join("libexec/deep/helper.txt"), with_tail(&fat)).unwrap();
    let fat45 = fat_header(0xCAFE_BABE, 45, true);
    std::fs::write(d.join("libexec/rg"), with_tail(&fat45)).unwrap();
    let fat200 = fat_header(0xCAFE_BABE, 200, true);
    std::fs::write(d.join("libexec/deep/zsh"), with_tail(&fat200)).unwrap();
    std::fs::write(d.join("README"), b"not code").unwrap();
    std::fs::write(d.join("NOTICE.md"), b"# Notices\n").unwrap();
    std::fs::write(d.join("bin/elf"), with_tail(b"\x7fELF")).unwrap();
    std::fs::write(d.join("empty"), b"").unwrap();
    assert_eq!(
        find_macho(&d).unwrap(),
        vec![
            d.join("bin/tool"),
            d.join("libexec/deep/helper.txt"),
            d.join("libexec/deep/zsh"),
            d.join("libexec/rg"),
        ]
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_interpreter_script_is_refused() {
    let d = tmp("script");
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::create_dir_all(d.join("codex-path")).unwrap();
    std::fs::write(
        d.join("bin/codex"),
        with_tail(&0xCFFA_EDFEu32.to_be_bytes()),
    )
    .unwrap();
    std::fs::write(d.join("codex-path/rg"), b"#!/bin/sh\nexec evil\n").unwrap();
    match find_macho(&d) {
        Err(StageError::SignerRefused(m)) => {
            let path = Path::new("codex-path").join("rg").display().to_string();
            assert!(m.starts_with(&path), "{m}");
        }
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// A symlink resolving to a Mach-O is refused by name — inside the tree or out of it —
/// while a symlink to anything else, or to nothing, is left alone.
#[cfg(unix)]
#[test]
fn a_symlinked_macho_is_refused_and_other_links_are_not() {
    let d = tmp("links");
    let outside = tmp("links-outside");
    std::fs::write(
        outside.join("real"),
        with_tail(&0xFEED_FACFu32.to_be_bytes()),
    )
    .unwrap();
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::write(d.join("notes"), b"text").unwrap();
    std::os::unix::fs::symlink("../notes", d.join("bin/notes")).unwrap();
    std::os::unix::fs::symlink("../missing", d.join("bin/dangling")).unwrap();
    assert_eq!(find_macho(&d).unwrap(), Vec::<PathBuf>::new());

    std::os::unix::fs::symlink(outside.join("real"), d.join("bin/claude")).unwrap();
    match find_macho(&d) {
        Err(StageError::SignerRefused(m)) => {
            assert!(m.starts_with("bin/claude "), "{m}");
        }
        other => panic!("{other:?}"),
    }
    std::fs::remove_file(d.join("bin/claude")).unwrap();

    std::fs::write(d.join("real"), with_tail(&0xFEED_FACFu32.to_be_bytes())).unwrap();
    std::os::unix::fs::symlink("../real", d.join("bin/claude")).unwrap();
    assert!(matches!(find_macho(&d), Err(StageError::SignerRefused(_))));
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&outside);
}

/// The fish row's shape: libSystem, an `LC_LOAD_DYLINKER` and an `LC_UUID` beside the
/// dylib a Homebrew-linked build loads.
fn homebrew_pcre2_fish(little: bool) -> Vec<u8> {
    thin(
        &[
            other_command(0xE, 32, little),
            other_command(0x1B, 24, little),
            dylib_command(0xC, "/usr/lib/libSystem.B.dylib", little),
            dylib_command(0xC, "/opt/homebrew/lib/libpcre2-32.dylib", little),
        ],
        little,
    )
}

#[test]
fn a_homebrew_pcre2_link_is_foreign_and_libsystem_alone_is_not() {
    let d = tmp("pcre2");
    let clean = thin(
        &[
            other_command(0xE, 32, true),
            dylib_command(0xC, "/usr/lib/libSystem.B.dylib", true),
            dylib_command(
                0x8000_0018,
                "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
                true,
            ),
        ],
        true,
    );
    std::fs::write(d.join("clean"), &clean).unwrap();
    assert_eq!(
        foreign_dylibs(&d.join("clean")).unwrap(),
        Vec::<String>::new()
    );
    let pcre2 = "/opt/homebrew/lib/libpcre2-32.dylib".to_owned();
    for little in [true, false] {
        std::fs::write(d.join("fish"), homebrew_pcre2_fish(little)).unwrap();
        assert_eq!(
            load_dylibs_at(&d.join("fish")).unwrap(),
            vec!["/usr/lib/libSystem.B.dylib".to_owned(), pcre2.clone()],
            "little={little}"
        );
        assert_eq!(
            foreign_dylibs(&d.join("fish")).unwrap(),
            vec![pcre2.clone()]
        );
    }
    for is_wide in [false, true] {
        let both = fat(&[clean.clone(), homebrew_pcre2_fish(true)], is_wide);
        std::fs::write(d.join("universal"), both).unwrap();
        assert_eq!(
            foreign_dylibs(&d.join("universal")).unwrap(),
            vec![pcre2.clone()]
        );
        std::fs::write(
            d.join("universal"),
            fat(std::slice::from_ref(&clean), is_wide),
        )
        .unwrap();
        assert_eq!(
            foreign_dylibs(&d.join("universal")).unwrap(),
            Vec::<String>::new()
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn every_loading_command_is_read_and_the_id_is_not() {
    let mut commands: Vec<Vec<u8>> = DYLIB_LOADS
        .iter()
        .map(|cmd| dylib_command(*cmd, &format!("/opt/x/{cmd:x}.dylib"), true))
        .collect();
    commands.push(dylib_command(0xD, "/opt/x/self.dylib", true));
    let names = load_names(&thin(&commands, true)).unwrap().loads;
    assert_eq!(names.len(), DYLIB_LOADS.len(), "{names:?}");
    assert!(
        !names.iter().any(|n| n.ends_with("self.dylib")),
        "{names:?}"
    );
}

/// A Homebrew `:any` dylib before relocation: its own name, what it loads and its run
/// paths all name the placeholder prefix, beside libSystem and an `LC_UUID`.
fn placeholder_dylib(little: bool) -> Vec<u8> {
    thin(
        &[
            dylib_command(
                DYLIB_ID,
                "@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib",
                little,
            ),
            other_command(0x1B, 24, little),
            dylib_command(0xC, "@@HOMEBREW_CELLAR@@/xz/1/lib/liblzma.5.dylib", little),
            rpath_command("@@HOMEBREW_PREFIX@@/lib", little),
            dylib_command(0xC, "/usr/lib/libSystem.B.dylib", little),
            rpath_command("@loader_path/../lib", little),
        ],
        little,
    )
}

#[test]
fn the_id_the_loads_and_the_rpaths_are_each_read() {
    let want = LoadNames {
        id: vec!["@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib".into()],
        loads: vec![
            "@@HOMEBREW_CELLAR@@/xz/1/lib/liblzma.5.dylib".into(),
            "/usr/lib/libSystem.B.dylib".into(),
        ],
        weak_loads: Vec::new(),
        rpaths: vec![
            "@@HOMEBREW_PREFIX@@/lib".into(),
            "@loader_path/../lib".into(),
        ],
    };
    for little in [true, false] {
        assert_eq!(load_names(&placeholder_dylib(little)).unwrap(), want);
    }
    let both = load_names(&fat(
        &[placeholder_dylib(true), placeholder_dylib(true)],
        true,
    ))
    .unwrap();
    assert_eq!(both.id, [want.id.clone(), want.id.clone()].concat());
    assert_eq!(both.rpaths.len(), 4);
    assert_eq!(both.once(), want, "the slices' names merge to one of each");
    let program = load_names(&homebrew_pcre2_fish(true)).unwrap();
    assert!(
        program.id.is_empty() && program.rpaths.is_empty(),
        "{program:?}"
    );
    assert_eq!(program.loads.len(), 2);
}

#[test]
fn a_malformed_rpath_command_is_refused() {
    let good = thin(&[rpath_command("@loader_path/../lib", true)], true);
    assert!(load_names(&good).is_ok(), "the unmutated fixture reads");
    let at = HEADER_64;
    let size = u32::from_le_bytes(good[at + 4..at + 8].try_into().unwrap()) as usize;
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut low = good.clone();
    low[at + 8..at + 12].copy_from_slice(&4u32.to_le_bytes());
    cases.push(("offset inside the fixed part", low));
    for offset in [8u32, 11] {
        let mut word = good.clone();
        word[at + 8..at + 12].copy_from_slice(&offset.to_le_bytes());
        cases.push(("offset inside the path's own offset word", word));
    }
    let mut high = good.clone();
    high[at + 8..at + 12].copy_from_slice(&(size as u32).to_le_bytes());
    cases.push(("offset at the command's end", high));
    let mut unterminated = good.clone();
    for b in &mut unterminated[at + RPATH_COMMAND..at + size] {
        *b = b'a';
    }
    cases.push(("no NUL", unterminated));
    let mut empty = good.clone();
    for b in &mut empty[at + RPATH_COMMAND..at + size] {
        *b = 0;
    }
    cases.push(("empty path", empty));
    cases.push((
        "under its fixed part",
        thin(&[other_command(RPATH, 8, true)], true),
    ));
    for (what, bytes) in cases {
        match load_names(&bytes) {
            Err(LoadError::Malformed(_)) => {}
            other => panic!("{what}: {other:?}"),
        }
    }
}

#[test]
fn a_32_bit_mach_o_reads_and_an_archive_slice_names_nothing() {
    let watch = |little: bool| {
        thin32(
            &[
                dylib_command32(DYLIB_ID, "@rpath/libswiftCore.dylib", little),
                other_command(0x1B, 24, little),
                dylib_command32(0xC, "/usr/lib/libSystem.B.dylib", little),
                rpath_command("@loader_path/../lib", little),
            ],
            little,
        )
    };
    assert_ne!(
        dylib_command32(DYLIB_ID, "@rpath/libswiftCore.dylib", true).len() % 8,
        0,
        "the fixture's command is 4- and not 8-aligned"
    );
    let want = LoadNames {
        id: vec!["@rpath/libswiftCore.dylib".into()],
        loads: vec!["/usr/lib/libSystem.B.dylib".into()],
        weak_loads: Vec::new(),
        rpaths: vec!["@loader_path/../lib".into()],
    };
    for little in [true, false] {
        assert_eq!(load_names(&watch(little)).unwrap(), want, "little={little}");
    }
    for is_wide in [false, true] {
        let universal = fat(&[watch(true), archive(), placeholder_dylib(true)], is_wide);
        let read = load_names(&universal).unwrap();
        assert_eq!(
            read.id,
            [
                "@rpath/libswiftCore.dylib",
                "@@HOMEBREW_PREFIX@@/opt/x/lib/libx.1.dylib"
            ]
        );
        assert_eq!(read.loads.len(), 3, "{read:?}");
        let archives = load_names(&fat(&[archive(), archive()], is_wide)).unwrap();
        assert_eq!(archives, LoadNames::default(), "a universal static archive");
    }
    let mut odd = watch(true);
    odd[HEADER_32 + 4..HEADER_32 + 8].copy_from_slice(&42u32.to_le_bytes());
    let four = thin(
        &[dylib_command32(0xC, "/usr/lib/libSystem.B.dylib", true)],
        true,
    );
    for (what, bytes) in [
        ("a 32-bit cmdsize not a multiple of 4", odd),
        ("a 64-bit cmdsize a multiple of 4 only", four),
        ("an archive alone", archive()),
        ("a 32-bit header cut short", watch(true)[..20].to_vec()),
    ] {
        match load_names(&bytes) {
            Err(LoadError::Malformed(_)) => {}
            other => panic!("{what}: {other:?}"),
        }
    }
}

#[test]
fn weak_only_loads_are_split_from_strong_loads() {
    let weak = 0x8000_0018;
    let bytes = thin(
        &[
            dylib_command(LOAD_DYLIB, "/p/lib/libstrong.dylib", true),
            dylib_command(weak, "/p/lib/libweak.dylib", true),
            dylib_command(weak, "/p/lib/libboth.dylib", true),
            dylib_command(LOAD_DYLIB, "/p/lib/libboth.dylib", true),
        ],
        true,
    );
    let names = load_names(&bytes).unwrap();
    assert_eq!(
        names.loads,
        [
            "/p/lib/libstrong.dylib",
            "/p/lib/libweak.dylib",
            "/p/lib/libboth.dylib",
            "/p/lib/libboth.dylib",
        ]
    );
    assert_eq!(
        names.weak_loads,
        ["/p/lib/libweak.dylib"],
        "a dual-loaded name is not weak-only"
    );
    assert_eq!(names.once().weak_loads, ["/p/lib/libweak.dylib"]);
}

/// The reader agrees with Apple's own `otool` on real files: a Command Line Tools
/// dylib with an install name and a run path, Apple's universal `/bin/ls` and this test
/// binary, every slice (`-arch all`), and where the developer tools carry them a thin
/// 32-bit dylib (Swift 5.0's arm64_32 `libswiftCore.dylib`), a universal dylib with a
/// 32-bit slice and a universal static archive (clang's runtime libraries, whose
/// archive `otool` lists no dylib for). The `otool` run is the developer directory's own
/// (read off `/var/db/xcode_select_link`, no process), never `/usr/bin/otool`, the shim
/// that raises Apple's install dialog where no developer tools are; with none, the
/// comparison ends COULD NOT RUN once the files it needs no tool for were read.
#[cfg(target_os = "macos")]
#[test]
fn the_reader_agrees_with_the_developer_tools_otool() {
    let developer = std::fs::read_link("/var/db/xcode_select_link")
        .unwrap_or_else(|_| PathBuf::from("/Library/Developer/CommandLineTools"));
    let me = std::env::current_exe().unwrap();
    let mut files = vec![PathBuf::from("/bin/ls"), me];
    for root in ["", "Toolchains/XcodeDefault.xctoolchain/"] {
        let lib = developer.join(root).join("usr/lib");
        let mut libs = vec![
            lib.join("libLTO.dylib"),
            lib.join("swift-5.0/watchos/libswiftCore.dylib"),
        ];
        for version in std::fs::read_dir(lib.join("clang")).into_iter().flatten() {
            let darwin = version.map(|v| v.path().join("lib/darwin"));
            if let Ok(darwin) = darwin {
                libs.push(darwin.join("libclang_rt.asan_watchos_dynamic.dylib"));
                libs.push(darwin.join("libclang_rt.osx.a"));
            }
        }
        files.extend(libs.into_iter().filter(|p| p.is_file()));
    }
    for file in &files {
        load_names_at(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    }
    let Some(otool) = [
        "usr/bin/otool",
        "Toolchains/XcodeDefault.xctoolchain/usr/bin/otool",
    ]
    .iter()
    .map(|rel| developer.join(rel))
    .find(|p| p.is_file()) else {
        aterm_uds::host::could_not_run_without(&["otool"]);
        return;
    };
    let run = |flag: &str, file: &Path| -> Vec<String> {
        let out = aterm_uds::spawnfd::command(&otool)
            .args(["-arch", "all", flag])
            .arg(file)
            .output()
            .expect("the developer tools' otool runs");
        assert!(out.status.success(), "otool {flag} {}", file.display());
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter_map(|line| match flag {
                "-l" => line.strip_prefix("path "),
                _ => (!line.ends_with(':') && !line.starts_with("Archive : ")).then_some(line),
            })
            .map(|name| name.split(" (").next().unwrap_or(name).to_owned())
            .collect()
    };
    let set = |names: Vec<String>| -> Vec<String> {
        let mut names = names;
        names.sort();
        names.dedup();
        names
    };
    let mut rpaths_seen = 0;
    for file in &files {
        let names = load_names_at(file).unwrap();
        let shown = file.display();
        assert_eq!(set(names.id.clone()), set(run("-D", file)), "-D {shown}");
        let listed = [names.id.clone(), names.loads.clone()].concat();
        assert_eq!(set(listed), set(run("-L", file)), "-L {shown}");
        let rpaths = run("-l", file);
        assert_eq!(set(names.rpaths.clone()), set(rpaths), "-l {shown}");
        rpaths_seen += names.rpaths.len();
    }
    assert!(
        files.len() < 3 || rpaths_seen > 0,
        "libLTO.dylib carries a run path: the -l comparison compared something"
    );
}

#[test]
fn only_the_os_roots_are_system_dylibs() {
    for ok in [
        "/usr/lib/libSystem.B.dylib",
        "/System/Library/Frameworks/Security.framework/Versions/A/Security",
    ] {
        assert!(is_system_dylib(ok), "{ok}");
    }
    for foreign in [
        "/opt/homebrew/lib/libpcre2-32.dylib",
        "/usr/local/lib/libpcre2-32.dylib",
        "@rpath/libpcre2-32.dylib",
        "@loader_path/../lib/x.dylib",
        "@executable_path/x.dylib",
        "/usr/lib/../../opt/homebrew/lib/libpcre2-32.dylib",
        "/System/./x.dylib",
        "/usr/libexec/x.dylib",
        "usr/lib/libSystem.B.dylib",
    ] {
        assert!(!is_system_dylib(foreign), "{foreign}");
    }
}

#[test]
fn malformed_load_commands_are_refused() {
    let good = homebrew_pcre2_fish(true);
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut zero = good.clone();
    zero[36..40].copy_from_slice(&0u32.to_le_bytes());
    cases.push(("cmdsize 0", zero));
    let mut odd = good.clone();
    odd[36..40].copy_from_slice(&12u32.to_le_bytes());
    cases.push(("cmdsize not a multiple of 8", odd));
    let mut long = good.clone();
    long[36..40].copy_from_slice(&4096u32.to_le_bytes());
    cases.push(("cmdsize past sizeofcmds", long));
    let mut count = good.clone();
    count[16..20].copy_from_slice(&5u32.to_le_bytes());
    cases.push(("ncmds past sizeofcmds", count));
    let mut sizeof = good.clone();
    sizeof[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push(("sizeofcmds past the file", sizeof));
    cases.push(("truncated header", good[..20].to_vec()));
    let dylib_at = HEADER_64 + 32 + 24;
    let mut low = good.clone();
    low[dylib_at + 8..dylib_at + 12].copy_from_slice(&8u32.to_le_bytes());
    cases.push(("name offset inside the fixed part", low));
    let mut high = good.clone();
    high[dylib_at + 8..dylib_at + 12].copy_from_slice(&4096u32.to_le_bytes());
    cases.push(("name offset past the command", high));
    let mut unterminated = good.clone();
    let size = u32::from_le_bytes(good[dylib_at + 4..dylib_at + 8].try_into().unwrap()) as usize;
    for b in &mut unterminated[dylib_at + DYLIB_COMMAND..dylib_at + size] {
        *b = b'a';
    }
    cases.push(("name without a NUL", unterminated));
    let mut empty = good.clone();
    for b in &mut empty[dylib_at + DYLIB_COMMAND..dylib_at + size] {
        *b = 0;
    }
    cases.push(("empty name", empty));
    let mut short_dylib = thin(&[other_command(0xC, 16, true)], true);
    short_dylib.truncate(HEADER_64 + 16 + 8);
    cases.push(("dylib command under its fixed part", short_dylib));
    cases.push(("not a Mach-O", b"#!/bin/sh\necho hi\n".to_vec()));
    cases.push(("empty arch table", fat(&[], false)));
    let mut many = fat(std::slice::from_ref(&good), false);
    many[4..8].copy_from_slice(&(MAX_SLICES + 1).to_be_bytes());
    cases.push(("too many slices", many));
    let mut outside = fat(std::slice::from_ref(&good), false);
    outside[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    cases.push(("slice outside the file", outside));
    let mut overlap = fat(std::slice::from_ref(&good), true);
    overlap[16..24].copy_from_slice(&0u64.to_be_bytes());
    cases.push(("slice over the header", overlap));
    cases.push((
        "universal inside universal",
        fat(&[fat(std::slice::from_ref(&good), false)], false),
    ));
    let mut swapped = fat(std::slice::from_ref(&good), false);
    swapped[..4].copy_from_slice(&0xBEBA_FECAu32.to_be_bytes());
    cases.push(("byte-swapped universal header", swapped));
    cases.push((
        "a Java class file",
        vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 0x34, 0, 0],
    ));
    for (what, bytes) in cases {
        match load_names(&bytes) {
            Err(LoadError::Malformed(_)) => {}
            other => panic!("{what}: {other:?}"),
        }
    }
    assert!(load_names(&good).is_ok(), "the unmutated fixture reads");
}

/// A real binary reads: the test binary itself and Apple's universal `/bin/ls` each
/// load libSystem and nothing outside the OS.
#[cfg(target_os = "macos")]
#[test]
fn real_binaries_read_as_self_contained() {
    let me = std::env::current_exe().unwrap();
    for path in [me.as_path(), Path::new("/bin/ls")] {
        let names = load_dylibs_at(path).unwrap();
        assert!(
            names.iter().any(|n| n == "/usr/lib/libSystem.B.dylib"),
            "{}: {names:?}",
            path.display()
        );
    }
    assert_eq!(
        foreign_dylibs(Path::new("/bin/ls")).unwrap(),
        Vec::<String>::new()
    );
    assert!(matches!(
        load_dylibs_at(Path::new("/bin")),
        Err(LoadError::Io(_))
    ));
}

/// A FIFO is refused without being opened — reading one would block the stage.
#[cfg(unix)]
#[test]
fn a_fifo_is_refused_without_being_read() {
    let d = tmp("fifo");
    let fifo = d.join("claude");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(matches!(find_macho(&d), Err(StageError::SignerRefused(_))));
    let _ = std::fs::remove_dir_all(&d);
}
