// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TOOLCHAIN, ONE SOURCE, STD ONLY.
//!
//! * [`discovery`] — THE toolchain: the directory `rust-toolchain.toml`'s `trust` pin
//!   resolves to, in the one resolution order the checks, `aterm help rust`, the libc
//!   oracle's gate and the release cutter share (its items are re-exported here).
//! * [`rustup`] — rustup's home and binary, and the listings the stock lanes read.
//! * [`build_cache`] — where plain `targo` puts a workspace's build intermediates, so
//!   a check run and a manual build share one cache.
//! * [`checkout`] — when a checkout is gone, and where nothing looks: the build cache
//!   sweep's rule and the disk watch's.
//! * [`lease`] — the flock protocol by which a run holds the toolchain it resolved for
//!   as long as it runs.
//! * [`layout`] — where under an atpkg prefix a build, its exec root and a view live.
//! * [`record_lock`] — a holder's lock that leaves its record readable (a lease's "who"
//!   line), on Windows too.
//!
//! atpkg re-exports each module under the path it always had (`atpkg::toolchain`,
//! `atpkg::build_cache`, `atpkg::build_cache::checkout`, `atpkg::lease`), adding only
//! what needs the rest of atpkg: the shim's `sh` half, the sweeps. atpkg's store
//! layout reads its directory names from [`layout`].

pub mod build_cache;
pub mod checkout;
pub mod discovery;
pub mod layout;
pub mod lease;
pub mod record_lock;
pub mod rustup;

pub use discovery::*;
