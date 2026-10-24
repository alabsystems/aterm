// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel-read hints from one managed shim exec. Capturing hints grants no route authority.

use std::path::{Path, PathBuf};

use crate::caller_shell::ProcArgs;
use crate::store::Layout;

/// The configured prefix and untrusted per-exec fields from one kernel process snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hints {
    prefix: PathBuf,
    pid: String,
    entry: PathBuf,
    target: PathBuf,
}

impl Hints {
    /// Capture only unique producer fields, without accepting their contents as authority.
    #[must_use]
    pub fn capture(layout: &Layout, args: &ProcArgs) -> Option<Self> {
        let value = |name: &str| {
            let prefix = format!("{name}=");
            let mut values = args.env.iter().filter_map(|v| v.strip_prefix(&prefix));
            let value = values.next()?;
            values.next().is_none().then(|| value.to_owned())
        };
        Some(Self {
            prefix: layout.prefix.clone(),
            pid: value("ATPKG_MANAGED_LAUNCH_PID")?,
            entry: value("ATPKG_MANAGED_LAUNCH_PATH")?.into(),
            target: value("ATPKG_MANAGED_LAUNCH_TARGET")?.into(),
        })
    }

    /// Whether this exact exec's physical image belongs to the independently produced
    /// managed route. Environment fields alone never confer launch-path authority.
    #[must_use]
    pub fn admits(&self, pid: u32, image: &Path, launch: &Path) -> bool {
        #[cfg(unix)]
        {
            let layout = Layout {
                prefix: self.prefix.clone(),
            };
            pid != 0
                && self.pid == pid.to_string()
                && launch.as_os_str() == self.entry.as_os_str()
                && normalized(&self.target)
                && std::fs::canonicalize(&self.target).ok().as_deref() == Some(image)
                && running_package(&layout, &self.target)
                && producer_target(&layout, launch).is_some()
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, image, launch);
            false
        }
    }
}

#[cfg(unix)]
fn normalized(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| !matches!(c, std::path::Component::ParentDir))
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
}

/// No traversal through a replaceable directory link inside the configured prefix.
#[cfg(unix)]
fn real_directories(layout: &Layout, end: &Path) -> bool {
    let Ok(rest) = end.strip_prefix(&layout.prefix) else {
        return false;
    };
    let mut at = layout.prefix.clone();
    std::iter::once(at.clone())
        .chain(rest.components().map(|part| {
            at.push(part);
            at.clone()
        }))
        .all(|path| std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir()))
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Origin {
    Store(u64),
    DevLink,
}

/// Both metadata files are bounded regular files, and the physical executable is the
/// package's declared entrypoint.
#[cfg(unix)]
fn package_root(target: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    if !normalized(target) {
        return None;
    }
    let root = crate::alab::alab_root_of_exe(target)?;
    let build = crate::metadata_io::read_bounded_regular_utf8(
        &root.join(crate::alab::ALAB_BUILD_JSON),
        64 * 1024,
    )
    .ok()?;
    if !aterm_json::from_str::<aterm_json::Value>(&build)
        .ok()?
        .is_object()
        || !std::fs::symlink_metadata(target)
            .is_ok_and(|m| m.file_type().is_file() && m.permissions().mode() & 0o111 != 0)
    {
        return None;
    }
    Some(root)
}

// A captured dev image can outlive its link marker; `producer_target` confers route authority.
/// A captured dev image can outlive its link marker: publication of B must not revoke
/// raw A's resume route. Its physical bytes are evidence only of the running image;
/// the independently configured current producer below confers route authority.
#[cfg(unix)]
fn running_package(layout: &Layout, target: &Path) -> bool {
    let Some(root) = package_root(target) else {
        return false;
    };
    let store_shaped = root.parent().is_some_and(|program| {
        program.file_name().is_some_and(|name| name == "codex")
            && program
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "store")
    });
    if store_shaped {
        matches!(package_origin(layout, target), Some(Origin::Store(_)))
    } else {
        true
    }
}

/// Only a store build in this prefix or the current recorded dev checkout can supply
/// a producer target. This is stricter than identifying a retained running image.
#[cfg(unix)]
fn package_origin(layout: &Layout, target: &Path) -> Option<Origin> {
    let root = package_root(target)?;
    if let Some((program, build)) = crate::ops::store_build_of(&layout.prefix, target)
        && program == "codex"
        && crate::alab::build_is_alab(layout, "codex", build)
        && std::fs::canonicalize(layout.build_dir("codex", build))
            .ok()
            .as_ref()
            == Some(&root)
        && real_directories(layout, target.parent()?)
    {
        return Some(Origin::Store(build));
    }
    let checkout = crate::linkmode::linked_checkout_checked(layout, "codex").ok()??;
    (std::fs::canonicalize(checkout).ok()? == root).then_some(Origin::DevLink)
}

/// Parse data only to re-render the actual producer. The complete byte comparison,
/// including its prelude, rejects an executable prefix shadowing a legitimate tail.
#[cfg(unix)]
fn producer_target(layout: &Layout, entry: &Path) -> Option<PathBuf> {
    let expected = crate::activate::exec_dir(layout, "codex").join("codex");
    if !normalized(&layout.prefix)
        || !normalized(entry)
        || entry.as_os_str() != expected.as_os_str()
        || !real_directories(layout, &layout.bin_dir())
    {
        return None;
    }
    let body = crate::metadata_io::read_bounded_regular_utf8(
        &layout.bin_dir().join("codex"),
        crate::platform::MAX_SHIM_BYTES,
    )
    .ok()?;
    let target = crate::platform::parse_sh_shim_target(&body)?;
    let prelude = match package_origin(layout, &target)? {
        Origin::Store(build) => {
            if crate::alab::current_build(layout, "codex") != Some(build) {
                return None;
            }
            let atpkg = crate::stub::sh_named_atpkg(&body, "__atpkg");
            if atpkg.as_deref().is_some_and(|p| !normalized(p)) {
                return None;
            }
            crate::platform::twin_prelude(
                "codex",
                &layout.prefix,
                atpkg.as_deref(),
                crate::selfupdate::verbs_of("codex"),
                &crate::yolo::policy_of("codex"),
            )
        }
        Origin::DevLink => String::new(),
    };
    let env = crate::platform::parse_sh_shim_env(&body);
    (crate::platform::sh_shim_content_managed(&target, &env, entry, &prelude) == body)
        .then_some(target)
}

/// At an admitted crash relaunch, restore only an absent managed Codex entry from the
/// current complete producer. Ordinary launch paths are untouched. No obstruction or
/// stale link is removed, and a concurrently selected image is never rolled back.
///
/// # Errors
/// A busy writer yields `WouldBlock`; invalid producer metadata, an obstruction, or an
/// I/O failure refuses the relaunch. Callers must not type the resume line on failure.
pub fn prepare_resume(layout: &Layout, launch: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::{Error, ErrorKind};
        let entry_dir = crate::activate::exec_dir(layout, "codex");
        if launch != entry_dir.join("codex") {
            return Ok(());
        }
        let _store = crate::lock::try_lock_store(layout).map_err(|error| match error {
            crate::lock::StoreLockError::Contended(_) => Error::new(ErrorKind::WouldBlock, error),
            error => Error::other(error),
        })?;
        let _publication = crate::lay::PUBLICATION_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let refused = || {
            Error::new(
                ErrorKind::PermissionDenied,
                "managed Codex producer does not admit this resume route",
            )
        };
        let target = producer_target(layout, launch).ok_or_else(refused)?;
        // The producer already owns this parent; do not create through a symlink or
        // replace a directory obstruction left by a person or another writer.
        if !real_directories(layout, &entry_dir) {
            return Err(refused());
        }
        match std::fs::symlink_metadata(launch) {
            Ok(meta)
                if meta.file_type().is_symlink()
                    && std::fs::read_link(launch).ok().as_ref() == Some(&target) =>
            {
                Ok(())
            }
            Ok(_) => Err(refused()),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                // Exclusive creation, unlike atomic_symlink: a raced obstruction is
                // never renamed over even when it appears after the lstat above.
                std::os::unix::fs::symlink(target, launch)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (layout, launch);
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "managed_launch_tests.rs"]
mod tests;
