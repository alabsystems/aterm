// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A save of a shared file's face in an Editor tab commits it
//! (docs/DESIGN-shared-files-2026-11-11.md §3, "Editing a face").
//!
//! The window takes no aterm-db dependency, so it cannot commit a face itself: the executable
//! that hosts it installs the commit here ([`install`]), and a durable save of a document whose
//! path is a face (`aterm_types::shared_file::name_of`) runs it with that path. The hook only
//! starts the commit; it never waits in the event loop. With no hook installed (a test's window,
//! a host that commits nothing) a save is a save and nothing more, and the face stays `edited`
//! until `aterm file commit <name>`.

use std::{path::Path, sync::OnceLock};

static COMMIT: OnceLock<fn(&Path) -> bool> = OnceLock::new();

/// Install the host's commit of a saved face, which answers whether it started one (a face under
/// another home's root is not this host's to commit). The first call wins: there is one host.
pub fn install(commit: fn(&Path) -> bool) {
    let _ = COMMIT.set(commit);
}

/// A document saved durably at `path`: where it is a shared file's face and the installed
/// commit started one, name the file it commits.
pub(crate) fn saved(path: &Path) -> Option<String> {
    let name = aterm_types::shared_file::name_of(path)?;
    let commit = COMMIT.get()?;
    commit(path).then_some(name)
}
