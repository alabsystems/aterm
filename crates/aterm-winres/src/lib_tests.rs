// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn sdk_versions_sort_numerically_newest_first() {
    let names = [
        "10.0.9600.0",
        "10.0.26100.0",
        "10.0.19041.0",
        "notes.txt",
        "10.0.1",
        "10.0.18446744073709551616.0",
    ];
    assert_eq!(
        sdk_versions_newest_first(names),
        ["10.0.26100.0", "10.0.19041.0", "10.0.9600.0"]
    );
    assert_eq!(
        sdk_versions_newest_first(["10.00.0.0", "10.0.0.0", "10.+0.1.2"]),
        ["10.+0.1.2", "10.00.0.0", "10.0.0.0"]
    );
    assert_eq!(
        sdk_versions_newest_first(["10.0.0.0", "10.00.0.0"]),
        ["10.0.0.0", "10.00.0.0"]
    );
}

#[test]
fn no_sdk_dirs_means_an_empty_list() {
    assert!(sdk_versions_newest_first(["x86", "arm64", "README"]).is_empty());
}

#[test]
fn dialect_follows_the_program_name() {
    assert_eq!(dialect_of(Path::new("C:/kits/x64/rc.exe")), Dialect::Rc);
    assert_eq!(dialect_of(Path::new("/usr/bin/llvm-rc")), Dialect::LlvmRc);
    assert_eq!(dialect_of(Path::new("LLVM-RC.EXE")), Dialect::LlvmRc);
    assert_eq!(
        dialect_of(Path::new("x86_64-w64-mingw32-windres")),
        Dialect::Windres
    );
    assert_eq!(dialect_of(Path::new("llvm-windres")), Dialect::Windres);
    assert_eq!(dialect_of(Path::new("windres-llvm-rc")), Dialect::Windres);
    assert_eq!(
        dialect_of(Path::new("/opt/windres/llvm-rc")),
        Dialect::LlvmRc,
        "only the final path component decides"
    );
    assert_eq!(
        dialect_of(Path::new("/opt/llvm-rc/custom-rc")),
        Dialect::Rc,
        "unrecognised names default to Rc"
    );
    assert_eq!(dialect_of(Path::new("")), Dialect::Rc);
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;
        let non_utf8 = Path::new(OsStr::from_bytes(b"windres-\xff"));
        assert_eq!(dialect_of(non_utf8), Dialect::Rc);
    }
}

#[test]
fn argument_shapes_per_dialect() {
    let inc = Path::new("inc");
    let rc = Path::new("inc/a.rc");
    let out = Path::new("out/a.res");
    assert_eq!(
        arguments(Dialect::Rc, inc, rc, out),
        ["/nologo", "/I", "inc", "/fo", "out/a.res", "inc/a.rc"]
    );
    assert_eq!(
        arguments(Dialect::LlvmRc, inc, rc, out),
        ["/I", "inc", "/FO", "out/a.res", "inc/a.rc"]
    );
    assert_eq!(
        arguments(Dialect::Windres, inc, rc, Path::new("out/a.o")),
        ["-I", "inc", "-i", "inc/a.rc", "-o", "out/a.o", "-O", "coff"]
    );
}

#[test]
fn artifact_extension_matches_what_the_linker_accepts() {
    assert_eq!(Dialect::Rc.artifact_extension(), "res");
    assert_eq!(Dialect::LlvmRc.artifact_extension(), "res");
    assert_eq!(Dialect::Windres.artifact_extension(), "o");
}

#[test]
fn outcome_display_names_the_state() {
    let linked = Outcome::Linked(PathBuf::from("out/a.res"));
    let not_win = Outcome::NotWindows;
    let no_comp = Outcome::NoCompiler("rc.exe on PATH".into());
    let failed = Outcome::Failed("rc.exe exited with 1: syntax error".into());

    assert!(linked.is_linked());
    assert!(!not_win.is_linked());
    assert!(!no_comp.is_linked());
    assert!(!failed.is_linked());

    assert!(!linked.is_failure());
    assert!(!not_win.is_failure());
    assert!(no_comp.is_failure());
    assert!(failed.is_failure());

    assert!(linked.to_string().ends_with("out/a.res"));
    assert!(!not_win.to_string().is_empty());
    assert!(no_comp.to_string().ends_with(": rc.exe on PATH"));
    assert!(
        failed
            .to_string()
            .ends_with(": rc.exe exited with 1: syntax error")
    );

    let rendered = [
        linked.to_string(),
        not_win.to_string(),
        no_comp.to_string(),
        failed.to_string(),
    ];
    for i in 0..rendered.len() {
        for j in (i + 1)..rendered.len() {
            assert_ne!(rendered[i], rendered[j]);
        }
    }
}

/// NEGATIVE CONTROL: Four-part Win32 `VS_FIXEDFILEINFO` version words must round-trip in little-endian word order without byte-swap or word transposition.
#[test]
fn winres_version_le_round_trips_and_lays_out_four_u32_words() {
    assert_eq!(encode_winres_version_le(0, 0, 0, 0), [0; 16]);
    assert_eq!(decode_winres_version_le([0; 16]), (0, 0, 0, 0));
    assert_eq!(
        encode_winres_version_le(u32::MAX, u32::MAX, u32::MAX, u32::MAX),
        [0xFF; 16]
    );
    assert_eq!(
        decode_winres_version_le([0xFF; 16]),
        (u32::MAX, u32::MAX, u32::MAX, u32::MAX)
    );

    let words = (0x0102_0304, 0x0506_0708, 0x090A_0B0C, 0x0D0E_0F10);
    let encoded = encode_winres_version_le(words.0, words.1, words.2, words.3);
    assert_eq!(
        encoded,
        [
            0x04, 0x03, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05, 0x0C, 0x0B, 0x0A, 0x09, 0x10, 0x0F,
            0x0E, 0x0D,
        ]
    );
    assert_eq!(decode_winres_version_le(encoded), words);
    assert_ne!(
        decode_winres_version_le(encode_winres_version_le(words.1, words.0, words.2, words.3)),
        words
    );
    for idx in 0..encoded.len() {
        let mut mutated = encoded;
        mutated[idx] ^= 0x01;
        assert_ne!(decode_winres_version_le(mutated), words);
    }
}
