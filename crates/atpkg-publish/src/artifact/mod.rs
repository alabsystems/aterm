// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Artifact building, staging, relocation, and signing for `atpkg-publish`.

pub mod arch;
pub mod bottle;
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
