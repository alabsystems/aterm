// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish`, the atpkg producer: it packs program builds and sysroot bundles, signs
//! their `pkg-<program>-<build>.toml`, publishes the channel's signed `index.toml` (making
//! every pack it pins public first), and runs the unattended publish lanes from a
//! snapshotted binary. `docs/DESIGN-atpkg-producer-rust.md` is the map.
//!
//! It is owner-side and never shipped: the `aterm` binary does not link it. It links the
//! client (`atpkg`) rather than copying it, so what it signs is what the client parses and
//! every self-check is the client's own verifier, in-process.
//!
//! # Layout
//!
//! * shared: [`cli`], [`ctx`], [`error`], [`api`], [`programs`], [`triple`], [`host`],
//!   [`quad`], [`pins`], [`identity`], [`git`], [`testkit`];
//! * [`artifact`] — building and signing what ships (pack, pack-bundle, build-local,
//!   sign);
//! * [`channel`] — the signed index and what it pins (index, mirror, publish-group,
//!   status);
//! * [`lane`] — the unattended lanes and their install.

pub mod api;
pub mod artifact;
pub mod channel;
pub mod cli;
pub mod ctx;
pub mod error;
pub mod git;
pub mod host;
pub mod identity;
pub mod lane;
pub mod pins;
pub mod programs;
pub mod quad;
pub mod testkit;
pub mod triple;
