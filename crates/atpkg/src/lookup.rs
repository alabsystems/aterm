// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only installed-tool lookup. This observes atpkg's own shim, not the shell's
//! PATH, and never launches it, provisions a pending tool, or authenticates a release.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::store::{Layout, ToolName};

/// Whether the observed atpkg executable is usable without installing anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// An executable was resolved. `managed` distinguishes store and development copies.
    Ready,
    /// No executable is installed, or its recorded target is gone.
    Missing,
    /// A pending stub exists; it was read, never invoked.
    Pending,
    /// The name or target cannot be admitted as an installed executable.
    Refused,
}

/// Locally recorded package provenance, not fresh signature or byte verification.
#[derive(Debug, Serialize)]
pub struct Provenance {
    /// When atpkg last wrote the status snapshot.
    pub recorded_at: String,
    /// Source recorded in that snapshot.
    pub index_source: String,
    /// Index pin recorded for this exact build, if it is still the recorded build.
    pub index_build: Option<u64>,
    /// Signed tree root recorded at installation, if this is the recorded build.
    pub tree_root: Option<String>,
}

/// Schema 1 of `aterm pkg which --json`.
#[derive(Debug, Serialize)]
pub struct Record {
    /// Structured lookup schema version.
    pub schema: u32,
    /// Requested installed tool name.
    pub tool: String,
    /// Local executable readiness, independent of release authorization.
    pub status: Status,
    /// Whether the executable belongs to this prefix's installed store build.
    pub managed: bool,
    /// Owning package and build, when the shim names this prefix's store.
    pub package: Option<String>,
    /// Store build identifier, not the tool's reported version.
    pub build: Option<u64>,
    /// Exact executable path atpkg would launch, preserving an alias's argv0 spelling.
    pub executable: Option<PathBuf>,
    /// Canonical backing file of `executable`, for byte identity rather than invocation.
    pub physical_executable: Option<PathBuf>,
    /// Physical execution root, which can differ from the stored payload root.
    pub physical_root: Option<PathBuf>,
    /// Physical immutable payload root for a managed build.
    pub store_root: Option<PathBuf>,
    /// Only the admitted environment declared by the shim; never the process environment.
    pub shim_env: BTreeMap<String, String>,
    /// Existing local status evidence, when available.
    pub provenance: Option<Provenance>,
    /// Why a record is not ready, or why its provenance is unavailable.
    pub reason: Option<&'static str>,
}

impl Record {
    fn new(tool: &str) -> Self {
        Self {
            schema: 1,
            tool: tool.to_string(),
            status: Status::Missing,
            managed: false,
            package: None,
            build: None,
            executable: None,
            physical_executable: None,
            physical_root: None,
            store_root: None,
            shim_env: BTreeMap::new(),
            provenance: None,
            reason: None,
        }
    }
}

/// Read the configured store without emitting configuration diagnostics or changing state.
#[must_use]
pub fn configured(tool: &str) -> Record {
    match crate::store::resolve_for_lookup() {
        Ok(layout) => read(layout.as_ref(), tool),
        Err(reason) => {
            let mut record = Record::new(tool);
            record.status = Status::Refused;
            record.reason = Some(reason);
            record
        }
    }
}

fn executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Physical owned payload root, admitting only the store's exact sibling `.noindex` alias.
/// This is path admission, not signed-content authentication or a lifetime lease.
#[must_use]
pub fn payload_root(layout: &Layout, package: &str, build: u64) -> Option<PathBuf> {
    let logical = layout.build_dir(package, build);
    let real_dir = |path: &Path| {
        std::fs::symlink_metadata(path)
            .is_ok_and(|meta| meta.is_dir() && !crate::platform::is_reparse(&meta))
    };
    if ![
        layout.prefix.as_path(),
        &layout.prefix.join("store"),
        logical.parent()?,
    ]
    .into_iter()
    .all(real_dir)
        || !crate::store::build_stands(&logical).ok()?
    {
        return None;
    }
    // The only admitted build indirection is atpkg's exact sibling .noindex
    // alias. build_stands uses that same layout contract on Unix.
    let physical = if aterm_toolchain::layout::is_linked_aside(&logical) {
        aterm_toolchain::layout::aside_of(&logical)?
    } else {
        logical
    };
    real_dir(&physical)
        .then(|| physical.canonicalize().ok())
        .flatten()
}

/// Observe one installed shim and its local metadata. No PATH fallback, process,
/// network request, lock, write, or repair is part of this lookup. A ready record is
/// a point-in-time observation: callers must hold a lease and verify identity before
/// relying on a tool throughout a build.
#[must_use]
pub fn read(layout: Option<&Layout>, tool: &str) -> Record {
    let mut record = Record::new(tool);
    if ToolName::new(tool).is_none() {
        record.status = Status::Refused;
        record.reason = Some("name is not admitted as an atpkg tool");
        return record;
    }
    let Some(layout) = layout else {
        record.reason = Some("the package prefix is unavailable");
        return record;
    };
    if crate::stub::pending_stub_exists(layout, tool) {
        record.status = Status::Pending;
        record.package = Some(crate::stub::program_of_in(layout, tool));
        record.reason = Some("the installed entry is a pending stub; nothing was launched");
        return record;
    }
    let Some((source, managed)) = crate::ops::managed_with_source(layout, tool) else {
        record.reason = Some("no installed atpkg shim resolves this tool");
        return record;
    };
    let owner = crate::ops::store_build_of(&layout.prefix, &source);
    if let Some((package, build)) = &owner {
        record.package = Some(package.clone());
        record.build = Some(*build);
    }
    if !managed.target.is_absolute() {
        record.status = Status::Refused;
        record.reason = Some("the installed executable path is not absolute");
        return record;
    }
    let Ok(target) = std::fs::canonicalize(&managed.target) else {
        record.reason = Some("the installed shim target is missing or unreadable");
        return record;
    };
    if !executable(&target) {
        record.status = Status::Refused;
        record.reason = Some("the installed target is not an executable regular file");
        return record;
    }
    if let Some((package, build)) = owner {
        let Some(root) = payload_root(layout, &package, build) else {
            record.status = Status::Refused;
            record.reason = Some("the installed payload root is absent or has unowned indirection");
            return record;
        };
        let execution_root = if target.starts_with(&root) {
            Some(root.clone())
        } else if package == crate::seam::SEAM_PROGRAM && managed.target != source {
            std::fs::canonicalize(crate::compat::root_dir(layout, build))
                .ok()
                .filter(|root| target.starts_with(root))
        } else {
            None
        };
        let Some(execution_root) = execution_root else {
            record.status = Status::Refused;
            record.reason = Some("the installed target escapes its package execution root");
            return record;
        };
        record.managed = true;
        record.store_root = Some(root);
        record.physical_root = Some(execution_root);
        match crate::status::read_checked(layout) {
            Ok(Some(status)) => {
                let row = status.programs.get(&package);
                let matches_build = row.is_some_and(|row| row.installed_build == Some(build));
                record.provenance = Some(Provenance {
                    recorded_at: status.updated_at,
                    index_source: status.index_source,
                    index_build: row
                        .and_then(|row| crate::state::managed_pin(&row.state))
                        .filter(|(pinned, _)| *pinned == build && matches_build)
                        .map(|(_, index)| index),
                    tree_root: row
                        .filter(|row| matches_build && !row.tree_root.is_empty())
                        .map(|row| row.tree_root.clone()),
                });
            }
            Ok(None) => record.reason = Some("no local package status provenance is recorded"),
            Err(_) => record.reason = Some("local package status provenance is unreadable"),
        }
    } else {
        record.reason = Some("the shim names an unmanaged or development-linked executable");
    }
    record.executable = Some(managed.target);
    record.physical_executable = Some(target);
    record.shim_env = managed.env.entries().iter().cloned().collect();
    record.status = Status::Ready;
    record
}

#[cfg(test)]
mod tests;
