// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TWO WINDOWS IMAGES, driven the way a shell drives them.
//!
//! The PE subsystem is a header field of one file, and a Windows shell keys on
//! it: it waits for a console-subsystem child and returns at once from a
//! GUI-subsystem one. So `aterm.exe` is the CONSOLE image (every verb printed
//! inline, piped, waited for) and `aterm-windowed.exe` — shipped as
//! `aterm-gui.exe` — the WINDOWED one (no console flash from the Start Menu),
//! and the console image hands every window it is asked for to the windowed
//! image, detached, with that image's stdio on NUL (`src/main.rs`,
//! `run_window`). These tests pin what a person at a prompt sees of that:
//!
//! * the two built files really carry the two subsystems (read from the PE
//!   header, since the attribute lives in source and the fact lives in the
//!   linked file);
//! * a window flag the window's parser REFUSES is refused by the console image,
//!   on its own stderr, exit 2 — handed to the sibling it printed into NUL and
//!   the console image exited 0 with no window (review 2026-09-27);
//! * a window flag the parser answers by PRINTING still prints through the
//!   console image.
//!
//! None of these opens a window or dials an instance: every argument list here
//! is refused or answered by the parser before the handoff, and none is a
//! plain launch the routing policy would forward.

#![cfg(windows)]

use std::io::Read as _;
use std::process::{Command, Output, Stdio};

/// `IMAGE_OPTIONAL_HEADER.Subsystem` of the PE file at `path`: `e_lfanew` at
/// 0x3C, then the 4-byte signature, the 20-byte COFF header, and the field at
/// offset 68 of the optional header (the same for PE32 and PE32+).
fn subsystem(path: &str) -> u16 {
    let mut head = vec![0u8; 4096];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .unwrap_or_else(|e| panic!("read the head of {path}: {e}"));
    let pe = u32::from_le_bytes(bytes::<4>(&head, 0x3C, path)) as usize;
    assert_eq!(
        &bytes::<4>(&head, pe, path),
        b"PE\0\0",
        "{path} is not a PE image"
    );
    u16::from_le_bytes(bytes::<2>(&head, pe + 4 + 20 + 68, path))
}

/// `N` bytes of `head` at `offset`, or a failure naming the file.
fn bytes<const N: usize>(head: &[u8], offset: usize, path: &str) -> [u8; N] {
    head.get(offset..offset + N)
        .and_then(|slice| slice.try_into().ok())
        .unwrap_or_else(|| panic!("{path}: offset {offset:#x} is past the first 4 KiB"))
}

/// Run the CONSOLE image with `args`, stdin on NUL (so the mode fork takes the
/// window route on its own, whatever `--window` says), everything captured.
fn console_image(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aterm"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run the console image")
}

#[test]
fn aterm_is_the_console_image_and_aterm_windowed_the_windowed_one() {
    const IMAGE_SUBSYSTEM_WINDOWS_GUI: u16 = 2;
    const IMAGE_SUBSYSTEM_WINDOWS_CUI: u16 = 3;
    assert_eq!(
        subsystem(env!("CARGO_BIN_EXE_aterm")),
        IMAGE_SUBSYSTEM_WINDOWS_CUI,
        "aterm.exe must be waited for by the shell"
    );
    assert_eq!(
        subsystem(env!("CARGO_BIN_EXE_aterm-windowed")),
        IMAGE_SUBSYSTEM_WINDOWS_GUI,
        "the windowed image must start without a console"
    );
}

#[test]
fn a_window_flag_the_parser_refuses_is_refused_by_the_console_image() {
    for (args, named) in [
        (&["--window", "--font-px", "abc"][..], "--font-px"),
        (&["--window", "--bogus"][..], "--bogus"),
        (&["--window", "--columns", "7"][..], "--columns"),
    ] {
        let out = console_image(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(2),
            "aterm {args:?} must exit 2 here; stderr: {stderr}"
        );
        assert!(
            stderr.contains(named),
            "aterm {args:?} names {named} on the console image's stderr: {stderr}"
        );
    }
}

#[test]
fn a_window_flag_the_parser_answers_prints_through_the_console_image() {
    let out = console_image(&["--window", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stdout: {stdout}");
    assert!(
        stdout.contains("USAGE"),
        "the window's --help reaches the caller: {stdout}"
    );
}
