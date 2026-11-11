// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The free-space floors (`docs/DESIGN-vz-guest-2026-10-23.md` §4).
//!
//! The live disk grows by up to one proof's build output plus the guest's caches, on a
//! volume other sessions share, so free space is read wherever it can run out.

use std::path::Path;

/// The floor at every bring-up, booted or reused, and before a bake or a clone: a bake
/// writes the provisioned guest (4.9 GiB) and a proof its build output (4.3 GiB), 9.2 GiB
/// together, doubled for the other sessions on the volume and rounded up.
pub const BRING_UP_FLOOR: u64 = 20 << 30;

/// The floor the helper stops a running VM at, read every [`crate::detect::TICK`]: about one
/// proof's build output, so no run meets a full volume.
pub const RUN_FLOOR: u64 = 4 << 30;

/// A volume's free bytes, read now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Free {
    /// Bytes free to an unprivileged writer.
    pub bytes: u64,
    /// The volume, as a report names it: its mount point and its pool.
    pub volume: String,
}

/// The volume holding `path`, read now (aterm-sysprobe's `volumes::read_path`). A path
/// that does not exist yet (the live disk while a bake runs) is read through its nearest
/// ancestor that does, resolved, so a symlink on the way (`/tmp`) cannot send the read to
/// another volume.
///
/// # Errors
/// The path is on no mount the table holds, or the volume cannot be read.
pub fn read(path: &Path) -> Result<Free, String> {
    let path = resolved(path);
    let table = aterm_sysprobe::volumes::table();
    let reading = aterm_sysprobe::volumes::read_path(&path, &table).map_err(|e| {
        format!(
            "could not read the free space of the volume holding {}: {e:?}",
            path.display()
        )
    })?;
    Ok(Free {
        bytes: reading.free,
        volume: format!("{} ({})", reading.mount.display(), reading.key.spelled()),
    })
}

/// `path` with its nearest existing ancestor resolved (`canonicalize`), and the rest of it,
/// which does not exist yet, appended unchanged.
fn resolved(path: &Path) -> std::path::PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |p, c| p.join(c));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// `bytes` in GiB, one decimal.
#[must_use]
pub fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / f64::from(1u32 << 30))
}

/// The NOT RUN words of a bring-up on `free`, below [`BRING_UP_FLOOR`], else `None`. The
/// bring-up has removed only the lane's own leftovers by then, and cloned or baked nothing.
#[must_use]
pub fn below_bring_up(free: &Free) -> Option<String> {
    (free.bytes < BRING_UP_FLOOR).then(|| {
        format!(
            "NOT RUN — the volume holding {}.img, {}, has {} free, under the {} a bring-up \
             needs (a bake writes 4.9 GiB and a proof 4.3 GiB, doubled for the other sessions \
             on the volume); nothing was cloned or baked",
            crate::layers::VM,
            free.volume,
            gib(free.bytes),
            gib(BRING_UP_FLOOR)
        )
    })
}

/// The words the helper ends every in-flight `Out` with when it stops a running VM on
/// `free`, below [`RUN_FLOOR`] (§4), else `None`. The run's verdict is the helper's.
#[must_use]
pub fn below_run(free: &Free) -> Option<String> {
    (free.bytes < RUN_FLOOR).then(|| {
        format!(
            "the volume holding {}.img has {} free; VM stopped",
            crate::layers::VM,
            gib(free.bytes)
        )
    })
}

#[cfg(test)]
#[path = "space_tests.rs"]
mod tests;
