// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Injected Windows path spellings only: no filesystem, process or platform call.

use super::*;

#[test]
fn ordinary_windows_native_spellings_keep_the_command_and_physical_hold_together() {
    for (program, physical) in [
        (
            r"C:\Users\person\claude.exe",
            r"\\?\C:\Users\person\claude.exe",
        ),
        (r"\\server\share\muse.exe", r"\\?\UNC\server\share\muse.exe"),
        (
            r"C:\Users\person's folder\agy.exe",
            r"\\?\C:\Users\person's folder\agy.exe",
        ),
        (r"\\?\C:\same\claude.exe", r"C:\same\claude.exe"),
    ] {
        let admitted = WindowsSpelling::admit(Path::new(program), Path::new(physical)).unwrap();
        assert_eq!(
            admitted.physical,
            Path::new(physical),
            "the hold stays physical"
        );
        {
            let word = ProbeWord::Help;
            let mut command = admitted.command();
            command.arg(word.word());
            assert_eq!(
                command.get_program(),
                program,
                "std keeps the original program/argv0 spelling"
            );
            assert_eq!(command.get_args().collect::<Vec<_>>(), [word.word()]);
        }
    }

    for (program, physical) in [
        (r"C:\alias\claude.exe", r"\\?\C:\other\claude.exe"),
        (r"C:\same\Alias.exe", r"\\?\C:\same\alias.exe"),
        (
            r"\\server\share\claude.exe",
            r"\\?\UNC\server\other\claude.exe",
        ),
        (r"C:\same\.\claude.exe", r"\\?\C:\same\claude.exe"),
        (r"C:\same\..\claude.exe", r"\\?\C:\claude.exe"),
        (r"C:\same\claude.exe.", r"\\?\C:\same\claude.exe"),
        (r"C:claude.exe", r"\\?\C:\same\claude.exe"),
        (r"\\.\C:\same\claude.exe", r"\\?\C:\same\claude.exe"),
        (r"C:\same\claude.exe", r"\\?\C:/same/claude.exe"),
    ] {
        assert!(
            WindowsSpelling::admit(Path::new(program), Path::new(physical)).is_none(),
            "an alias or ambiguous spelling stays refused: {program:?} -> {physical:?}"
        );
    }
}
