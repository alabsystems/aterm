// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! tools/install.sh, the `curl … | bash` bootstrap, under test.
//!
//! The installer stays shell (macOS's stock bash 3.2 is its floor) because it
//! runs on a machine with nothing installed; its tests are these. Each drives
//! the script itself — its functions through the library mode, or the whole
//! run under a scratch HOME
//! with the network and GitHub stubbed — and asserts on what it decided:
//! exit codes, the globals a function sets, files placed, requests made. The
//! differential tests hold the installer's channel grammar and its embedded
//! Linux verifier to the updater's Rust implementation on generated corpora.
//!
//! `targo --unverified test -p aterm-update --test install_sh`
//!
//! Unix only: the script is bash, and the tests lay unix stand-ins and modes.
#![cfg(unix)]

mod channel;
mod decisions;
mod grammar;
mod guard;
mod linux_verify;
mod options;
mod ownership;
mod source_build;
mod support;
