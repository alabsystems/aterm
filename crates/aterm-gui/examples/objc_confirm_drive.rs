// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drives owner and general menu confirmations on real `NSAlert`s through the macOS runtime
//! adapter, checking their words, buttons, clicks, Return/keypad Enter defaults and Escape in process.
//! `--rename-only` selects the machine-name panel without unrelated confirmation rounds.
//! `--snapshots DIR` saves its pending and synchronized naming fixtures as native content PNGs.

/// The drive could not execute here; not a pass.
#[cfg(not(target_os = "macos"))]
const NOT_RUN: u8 = 2;

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "objc-confirm-drive: NOT RUN — this drive is about \
         crates/aterm-gui/src/menu.rs's AppKit confirmation, which does not exist off macOS."
    );
    std::process::ExitCode::from(NOT_RUN)
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (rename_only, snapshots) = match args.as_slice() {
        [] => (false, None),
        [flag] if flag == "--rename-only" => (true, None),
        [flag, dir]
            if flag == "--snapshots"
                && !dir.is_empty()
                && !dir.as_encoded_bytes().starts_with(b"--") =>
        {
            (false, Some(std::path::Path::new(dir)))
        }
        [rename, flag, dir]
            if rename == "--rename-only"
                && flag == "--snapshots"
                && !dir.is_empty()
                && !dir.as_encoded_bytes().starts_with(b"--") =>
        {
            (true, Some(std::path::Path::new(dir)))
        }
        [flag, dir, rename]
            if flag == "--snapshots"
                && !dir.is_empty()
                && !dir.as_encoded_bytes().starts_with(b"--")
                && rename == "--rename-only" =>
        {
            (true, Some(std::path::Path::new(dir)))
        }
        _ => {
            eprintln!("usage: objc_confirm_drive [--rename-only] [--snapshots DIR]");
            return std::process::ExitCode::from(2);
        }
    };
    std::process::ExitCode::from(
        u8::try_from(aterm_gui::run_confirm_drive(rename_only, snapshots)).unwrap_or(1),
    )
}
