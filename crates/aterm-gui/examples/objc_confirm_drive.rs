// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drives owner and general menu confirmations on real `NSAlert`s through the macOS runtime
//! adapter, checking their words, buttons, clicks, Return/keypad Enter defaults and Escape in process.
//! `--rename-only` selects the machine-name panel without unrelated confirmation rounds.

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
    let rename_only = match args.as_slice() {
        [] => false,
        [flag] if flag == "--rename-only" => true,
        _ => {
            eprintln!("usage: objc_confirm_drive [--rename-only]");
            return std::process::ExitCode::from(2);
        }
    };
    std::process::ExitCode::from(
        u8::try_from(aterm_gui::run_confirm_drive(rename_only)).unwrap_or(1),
    )
}
