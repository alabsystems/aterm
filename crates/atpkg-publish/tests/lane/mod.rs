// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Work package 3's tests: the Apple and Linux lanes' decisions, state, latches and
//! holds (over fake packers, indexers and hosts), the commit resolution on real git
//! graphs, the snapshot self-check, the install, and the rendered launchd/systemd units.

mod alab;
mod fixture;
mod install;
mod linux;
mod resolve;
mod state;
