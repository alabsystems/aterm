// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A scratch world: the private HOME, configuration and runtime folders a check, a measurement,
//! a smoke or a lab starts an aterm in. That aterm is never the person's, so it starts none of the
//! automatic lanes that act for the login's own account or reach the person's real LAN, and none
//! that can put an OS consent prompt on the person's desk: macOS keys the firewall's
//! incoming-connection question and a background item to the account and the app, never to
//! HOME, so a world cannot answer them privately and must not ask them at all.
//!
//! One layout says what a world is, in the two places the lanes read, because a release build of
//! aterm (no development seams, [`crate::dev_seam!`]) reads only some of them:
//! - its environment ([`Layout::env`]) carries [`MARKER`] at [`Layout::state`], which the lanes
//!   that reach the local network read as a plain environment variable in every build: the
//!   converge runner's window lane, the `_alab._tcp` browse, the Commons runtime, and the node
//!   job's updater and account claims. That folder is also the state root a build without seams
//!   resolves from the world's own variables ([`crate::dirs::resolve_state_dir_for`]), on every
//!   platform;
//! - its `aterm.toml` ([`Config::render`]) turns off every [`Lane`] the site does not drive, each
//!   lane's table the product's own switch for it.
//!
//! The fleet listener reads neither: which vault it reads follows where its state root resolved
//! ([`crate::dirs::runs_under_private_state_root`]), so a release build in a world reads its
//! HOME's vault, and `[swarm]` off keeps it from listening.
//!
//! Lay a world out with [`Layout`] ([`Layout::under`] a root, or [`Layout::for_platform`] for a VM
//! guest or Windows from any host) and write its `aterm.toml` with [`Config`]. A site keeps only
//! its own head (its root keys, then its tables), the lanes it drives (by [`Lane`], never by
//! hand-written tables), where its root lives and for how long, its arguments and how it spawns.
//! [`Layout::make`] writes the world, and [`Layout::command`] starts a program in it.
//!
//! The layout is a `std`-only file of its own (`world_layout.rs`), which this module re-exports,
//! so the wrapper a macOS release link runs, which links no crate, mounts it by `#[path]`
//! (`probe_world` in `crates/aterm/src/macho_order.rs`). What needs a crate is here, off wasm:
//! [`Layout::live_env`] and [`Layout::command`].

#[path = "world_layout.rs"]
mod layout;

pub use layout::*;

#[cfg(not(target_family = "wasm"))]
use std::ffi::{OsStr, OsString};

#[cfg(not(target_family = "wasm"))]
impl Layout {
    /// [`Layout::env`] with this process's environment as its parent, then the mark of the verify
    /// run and of the trust-lane check this process runs under where it carries them
    /// ([`aterm_uds::process::VERIFY_RUN_VAR`], [`aterm_uds::process::TRUST_CHECK_VAR`]), so that
    /// run still counts the world's processes among its own.
    ///
    /// # Errors
    /// [`Layout::env`]'s.
    pub fn live_env(&self) -> Result<Vec<(&'static str, OsString)>, LayoutRefusal> {
        let mut env = self.env(&|name| std::env::var_os(name))?;
        for mark in [
            aterm_uds::process::VERIFY_RUN_VAR,
            aterm_uds::process::TRUST_CHECK_VAR,
        ] {
            if let Some(value) = std::env::var_os(mark) {
                env.push((mark, value));
            }
        }
        Ok(env)
    }

    /// A command that starts `program` in the world: [`Layout::live_env`] its whole environment,
    /// HOME its folder.
    ///
    /// # Errors
    /// [`Layout::env`]'s.
    pub fn command(
        &self,
        program: impl AsRef<OsStr>,
    ) -> Result<aterm_uds::spawnfd::Command, LayoutRefusal> {
        let env = self.live_env()?;
        let mut command = aterm_uds::spawnfd::command(program);
        command.env_clear().envs(env).current_dir(&self.home);
        Ok(command)
    }
}

#[cfg(test)]
#[path = "world_layout_tests.rs"]
mod tests;
