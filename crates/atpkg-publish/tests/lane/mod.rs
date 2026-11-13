// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lanes: the Apple, Linux and tip lanes' decisions, state, latches and holds (over fake
//! packers, indexers, hosts and a scripted qualification backend), the ready dispatcher, commit
//! resolution on real git graphs, the snapshot self-check, the install, and the rendered
//! launchd/systemd units.

mod alab;
mod fixture;
mod host_choice;
mod install;
mod linux;
mod resolve;
mod state;
mod tip;
