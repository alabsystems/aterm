// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! tools/install.sh, the `curl … | bash` bootstrap, under test.
#![cfg(unix)]

// Enable at Startup is a launchd job, read with macOS's plutil.
#[cfg(target_os = "macos")]
mod caffeine;
// The signed-release corpus both of install.sh's Linux verifiers are held to; the
// static one's suite (crates/aterm-bootstrap-verify/tests/install_sh) includes it by
// path. Linux's here, as the embedded verifier it holds is.
#[cfg(target_os = "linux")]
mod chain;
mod channel;
mod decisions;
mod grammar;
mod guard;
#[cfg(target_os = "linux")]
mod linux_verify;
mod options;
mod ownership;
mod root_run;
mod source_build;
mod support;
