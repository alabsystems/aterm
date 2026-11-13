// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::program_in;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

const ROSETTA: &[u8] = b"/mnt/lima-rosetta/rosetta";
/// A translated process's `auxv` pairs, as measured: `AT_EXECFD` (2) names descriptor 3.
const TRANSLATED: &[(u64, u64)] = &[(33, 275_810_927_144_960), (6, 4096), (2, 3), (0, 0)];
/// A native process's: no `AT_EXECFD`.
const NATIVE: &[(u64, u64)] = &[(33, 281_474_842_492_928), (6, 4096), (0, 0)];

/// `auxv` as the kernel writes it: native-endian 64-bit `(type, value)` pairs.
fn auxv(pairs: &[(u64, u64)]) -> Vec<u8> {
    pairs
        .iter()
        .flat_map(|&(kind, value)| [kind.to_ne_bytes(), value.to_ne_bytes()])
        .flatten()
        .collect()
}

/// Writes `pid`'s directory under `proc`: its `exe` link, its `auxv` (none at all for
/// `None`), descriptor 3's link and `fdinfo` when it holds one, and its `maps`.
fn process(
    proc: &Path,
    pid: i32,
    exe: &[u8],
    pairs: Option<&[(u64, u64)]>,
    handed: Option<(&[u8], &str)>,
    maps: &[u8],
) {
    let dir = proc.join(pid.to_string());
    std::fs::create_dir_all(dir.join("fd")).expect("its fd directory");
    std::fs::create_dir_all(dir.join("fdinfo")).expect("its fdinfo directory");
    std::os::unix::fs::symlink(OsStr::from_bytes(exe), dir.join("exe")).expect("its exe");
    if let Some(pairs) = pairs {
        std::fs::write(dir.join("auxv"), auxv(pairs)).expect("its auxv");
    }
    if let Some((link, info)) = handed {
        std::os::unix::fs::symlink(OsStr::from_bytes(link), dir.join("fd/3"))
            .expect("its descriptor 3");
        std::fs::write(dir.join("fdinfo/3"), info).expect("its fdinfo");
    }
    std::fs::write(dir.join("maps"), maps).expect("its maps");
}

/// `maps` rows of the file `shown` (the path as `maps` prints it, a newline as `\012`) with
/// inode `ino`, its `r-xp` row only when `executable`, then the translator's own.
fn rows(shown: &[u8], ino: u64, executable: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for (range, perms) in [
        ("555555554000-555555555000", "r--p"),
        ("555555555000-555555556000", "r-xp"),
        ("555555558000-555555559000", "rw-p"),
    ] {
        if perms == "r-xp" && !executable {
            continue;
        }
        out.extend_from_slice(
            format!("{range} {perms} 00001000 fd:01 {ino}                     ").as_bytes(),
        );
        out.extend_from_slice(shown);
        out.push(b'\n');
    }
    out.extend_from_slice(
        b"7fffff326000-7fffff34e000 r-xp 00000000 fd:01 525283                     \
          /mnt/lima-rosetta/rosetta\n",
    );
    out
}

/// An `fdinfo` of the kernel's exec open of inode `ino`, as measured.
fn exec_open(ino: u64) -> String {
    format!("pos:\t64\nflags:\t0400040\nmnt_id:\t32\nino:\t{ino}\n")
}

/// A translated process (Rosetta's exe, `AT_EXECFD` 3 holding the kernel's exec open of a
/// program it maps `r-xp`) reads as that program, under `binfmt_misc`'s `O` flag whatever
/// path the registration names its interpreter by (Debian's qemu one names a link), and so
/// does one whose path holds a newline, which `maps` prints as `\012`. A process the kernel
/// handed no descriptor reads as its `exe`, a registered interpreter run natively
/// (`python3.12` under `binfmt-support`) and a translator registered with no `O` flag
/// included, as does one whose `auxv` does not read. NEGATIVE CONTROLS, one per conjunct,
/// each `None`: an `exe` the kernel marks ` (deleted)`; descriptor 3 gone; a program only
/// mapped readable; a descriptor link the kernel marks ` (deleted)`, though `maps` marks
/// its rows alike; a descriptor without the exec mark; an exec open of another file than the
/// one the link names (a library's descriptor swapped between the reads); a link that is
/// no path (`pipe:[…]`), though `maps` shows it, nor a relative one, though it names a file
/// from the directory the tests run in.
#[test]
fn a_process_reads_as_the_program_its_own_proc_directory_names() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let proc = root.path().join("proc");
    let registered = proc.join("sys/fs/binfmt_misc");
    std::fs::create_dir_all(&registered).expect("binfmt_misc");
    for (name, text) in [
        ("status", "enabled\n"),
        (
            "rosetta",
            "enabled\ninterpreter /mnt/lima-rosetta/rosetta\nflags: OCF\noffset 0\n",
        ),
        (
            "python3.12",
            "enabled\ninterpreter /usr/bin/python3.12\nflags: \noffset 0\n",
        ),
        (
            "qemu-x86_64",
            "enabled\ninterpreter /usr/libexec/qemu-binfmt/x86_64-binfmt-P\nflags: POCF\n",
        ),
    ] {
        std::fs::write(registered.join(name), text).expect("a registration");
    }
    let program = |dir: &str| -> (PathBuf, u64) {
        let dir = root.path().join(dir);
        std::fs::create_dir_all(&dir).expect("a program's directory");
        let file = dir.join("probe");
        std::fs::write(&file, b"\x7fELF").expect("a program");
        let ino = std::fs::metadata(&file).expect("its inode").ino();
        (file, ino)
    };
    let (probe, ino) = program("bin");
    let (_library, library_ino) = program("lib");
    let (odd, odd_ino) = program("a\nb");
    let probe_bytes = probe.as_os_str().as_bytes();
    let open = exec_open(ino);
    let mapped = rows(probe_bytes, ino, true);
    let translated = |pid, exe: &[u8], link: &[u8], info: &str, maps: &[u8]| {
        process(&proc, pid, exe, Some(TRANSLATED), Some((link, info)), maps);
    };
    let native =
        |pid, exe: &[u8]| process(&proc, pid, exe, Some(NATIVE), None, &rows(exe, 7, true));
    let mut wrong = Vec::new();
    let mut read = |pid, want: Option<&[u8]>, what: &str| {
        let (got, want) = (
            program_in(&proc, pid),
            want.map(|w| PathBuf::from(OsStr::from_bytes(w))),
        );
        if got != want {
            wrong.push(format!("{what} (pid {pid}): {got:?}, not {want:?}"));
        }
    };

    translated(10, ROSETTA, probe_bytes, &open, &mapped);
    read(10, Some(probe_bytes), "a translated program");
    let qemu: &[u8] = b"/usr/bin/qemu-x86_64-static";
    translated(11, qemu, probe_bytes, &open, &mapped);
    read(11, Some(probe_bytes), "an O registration by a link");
    let odd_bytes = odd.as_os_str().as_bytes();
    let shown = [root.path().as_os_str().as_bytes(), b"/a\\012b/probe"].concat();
    translated(
        12,
        ROSETTA,
        odd_bytes,
        &exec_open(odd_ino),
        &rows(&shown, odd_ino, true),
    );
    read(12, Some(odd_bytes), "a newline in the program's path");

    let python: &[u8] = b"/usr/bin/python3.12";
    native(20, python);
    read(20, Some(python), "native python");
    native(21, qemu);
    read(21, Some(qemu), "no O flag");
    process(
        &proc,
        22,
        ROSETTA,
        None,
        Some((probe_bytes, open.as_str())),
        &mapped,
    );
    read(22, Some(ROSETTA), "no auxv");

    native(30, b"/usr/bin/sleep (deleted)");
    read(30, None, "a deleted exe");
    process(&proc, 31, ROSETTA, Some(TRANSLATED), None, &mapped);
    read(31, None, "descriptor 3 gone");
    translated(
        32,
        ROSETTA,
        probe_bytes,
        &open,
        &rows(probe_bytes, ino, false),
    );
    read(32, None, "a program mapped only readable");
    let gone = [probe_bytes, b" (deleted)"].concat();
    translated(33, ROSETTA, &gone, &open, &rows(&gone, ino, true));
    read(33, None, "a deleted program");
    let plain = format!("pos:\t0\nflags:\t0400000\nmnt_id:\t32\nino:\t{ino}\n");
    translated(34, ROSETTA, probe_bytes, &plain, &mapped);
    read(34, None, "no exec mark");
    translated(35, ROSETTA, probe_bytes, &exec_open(library_ino), &mapped);
    read(35, None, "another file's exec open");
    translated(
        36,
        ROSETTA,
        b"pipe:[4026]",
        &open,
        &rows(b"pipe:[4026]", ino, true),
    );
    read(36, None, "no path");
    let near = b"Cargo.toml";
    let near_ino = std::fs::metadata(OsStr::from_bytes(near))
        .expect("this crate's manifest, beside the tests")
        .ino();
    translated(
        37,
        ROSETTA,
        near,
        &exec_open(near_ino),
        &rows(near, near_ino, true),
    );
    read(37, None, "a relative path");
    assert!(wrong.is_empty(), "{wrong:#?}");
}
