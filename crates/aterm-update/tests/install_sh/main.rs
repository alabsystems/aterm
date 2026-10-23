// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! tools/install.sh, the `curl … | bash` bootstrap, under test.
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
