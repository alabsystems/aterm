// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Windows console and windowed PE images.

#![cfg(windows)]

use std::io::Read as _;
use std::process::{Output, Stdio};

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

fn bytes<const N: usize>(head: &[u8], offset: usize, path: &str) -> [u8; N] {
    head.get(offset..offset + N)
        .and_then(|slice| slice.try_into().ok())
        .unwrap_or_else(|| panic!("{path}: offset {offset:#x} is past the first 4 KiB"))
}

fn console_image(args: &[&str]) -> Output {
    aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_aterm"))
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
