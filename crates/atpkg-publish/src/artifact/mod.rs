// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Building and signing what ships.
//!
//! | module | does |
//! |---|---|
//! | [`pack`] | `pack`: one program build for one target, built, gated, staged and signed |
//! | [`bundle`] | `pack-bundle`: a sysroot bundle (trust, trust-mc), as a typestate |
//! | [`package`] | `pack-package`: a prebuilt package tree (codex-alab), packed whole and staged as a client stages it |
//! | [`closure`] | the trust bundle's frozen rustc-dev + rust-src closure |
//! | [`build_local`] | `build-local`: a development build, never signed |
//! | [`sign`] | `sign`: signs staged manifests, then verifies them with the client's rule |
//! | [`pkgdoc`] | the manifest writer (fresh, or merged onto a published base) |
//! | [`arch`] | the arch gate: each binary's own header against the claimed target |
//! | [`tarwrite`] | the deterministic `.tar.zst` writer and `disk_installed` |
//! | [`toolchain`] | the store's Trust toolchain, leased, its compiler's lineage checked |
//! | [`lineage`] | the payload scan over a staged tree, and the `LINEAGE` stamp a bundle carries |
//! | [`lineage_switch`] | the lineage receipt switch, read from the trust repo's live main |
//! | [`relocate`] | pack-time relocation: a staged payload made self-contained, fail-closed |
//! | [`upload`] | the staging upload and the public-immutability gate |
//! | [`tools`] | every external program a pack runs (`strip`, `codesign`, `lipo`, the build driver, probes) |

pub mod arch;
pub mod build_local;
pub mod bundle;
pub mod closure;
pub mod lineage;
pub mod lineage_switch;
pub mod pack;
pub mod package;
pub mod pkgdoc;
pub mod relocate;
pub mod sign;
pub mod tarwrite;
pub mod toolchain;
pub mod tools;
pub mod upload;
