// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `DYLD_INSERT_LIBRARIES` sleep-clock interposer for the real-Claude lab.

#[cfg(any(target_os = "macos", test))]
mod model;

#[cfg(all(target_os = "macos", not(test)))]
mod interpose;
