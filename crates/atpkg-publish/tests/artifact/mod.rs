// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The artifact tests: pack, pack-bundle, pack-package, closure, arch gate, tar writer,
//! manifest writer, toolchain resolution, the payload scan and LINEAGE stamp, the
//! lineage switch, build-local, sign.

mod arch;
mod build_local;
mod bundle;
mod closure;
mod e2e;
mod fixture;
mod lineage;
mod lineage_switch;
mod pack;
// Real `codesign` on a real Mach-O, and the client's macOS stage.
#[cfg(target_os = "macos")]
mod package;
mod pkgdoc;
mod sign;
mod tarwrite;
mod toolchain;
