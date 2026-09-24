// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Where the app updater's `…/aterm/Updates` root lives — one derivation shared by
//! `aterm-update`'s staging layout and the tests and tools that read it.
//!
//! The module keeps its historical name. It used to hold the "a live process is
//! reading this bundle's sealed payload" marker (`Updates/toolchain-install`) as well:
//! `atpkg seed` claimed it while it extracted the batteries-included toolchain seed a
//! release cut had sealed inside the app bundle, and the app updater deferred its
//! bundle swap while a live pid held it, so a swap to the lean updater zip (which
//! carries no seal) could not tear a multi-GB extraction mid-read. The producer was
//! retired 2026-08-26, no release since v0.63.0 seals a seed, and a pre-v0.63 bundle
//! updates itself to a lean one, so no live install could ever claim the marker; Phase 5
//! of `docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md` deleted the reader lane,
//! the marker and the deferral together. [`updates_root`] had other callers and stays.

use std::path::PathBuf;

/// The `…/aterm/Updates` root shared with `aterm-update`'s staging layout: the
/// HOME-keyed Application Support base, which a DEVELOPMENT build may relocate with the
/// `ATERM_UPDATE_ROOT` seam (test/demo isolation; `aterm_types::dev_seam!` — a shipped
/// binary does not read it, and a scratch `$HOME` isolates it there). `None` when HOME
/// is unset or the directory cannot be made private.
#[must_use]
pub fn updates_root() -> Option<PathBuf> {
    let base = match aterm_types::dev_seam!("ATERM_UPDATE_ROOT") {
        Some(root) if !root.is_empty() => PathBuf::from(root),
        _ => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("aterm")
        }
    };
    let root = base.join("Updates");
    crate::ensure_private_dir(&root).ok()?;
    Some(root)
}
