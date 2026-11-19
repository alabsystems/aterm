// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Check Trust's optional runtime tools and preserve the arm64 Darwin worker's paired bytes.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use atpkg::lineage::CompilerContent;

use crate::error::{Error, Result};
use crate::triple::{AARCH64_APPLE_DARWIN, Triple};

const ROOT_LINKER: &str = "bin/rust-lld";
const HOST_LINKER: &str = "lib/rustlib/aarch64-apple-darwin/bin/rust-lld";

#[derive(Default)]
pub(super) struct DarwinRuntime(BTreeMap<PathBuf, String>);

impl DarwinRuntime {
    pub(super) fn capture(
        program: &str,
        triple: Triple,
        compiler: &CompilerContent,
    ) -> Result<Self> {
        if program != "trust" || triple != AARCH64_APPLE_DARWIN {
            return Ok(Self::default());
        }
        let root = &compiler.root;
        let mut files = BTreeMap::from([
            (PathBuf::from("bin/trustc"), compiler.trustc_sha256.clone()),
            (PathBuf::from("bin/targo"), compiler.targo_sha256.clone()),
            (
                PathBuf::from(&compiler.driver_rel),
                compiler.driver_sha256.clone(),
            ),
        ]);
        let debug_tool = "bin/dsymutil";
        if executable_helper(root, debug_tool)?.is_some() {
            files.insert(PathBuf::from(debug_tool), digest(&root.join(debug_tool))?);
        }
        if has_paired_linker(root)? {
            let host = digest(&root.join(HOST_LINKER))?;
            if digest(&root.join(ROOT_LINKER))? != host {
                return Err(Error::refused(format!(
                    "native Trust linker {ROOT_LINKER} differs from its producer copy at {HOST_LINKER}"
                )));
            }
            files.insert(PathBuf::from(ROOT_LINKER), host.clone());
            files.insert(PathBuf::from(HOST_LINKER), host);
        }

        // Trust's worker selects @rpath leaves from these two directories. Keep
        // every native image there, including dependencies of the compiler driver.
        for directory in [
            Path::new("lib"),
            Path::new("lib/rustlib/aarch64-apple-darwin/lib"),
        ] {
            let dir = root.join(directory);
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(Error::io(dir.display(), &e)),
            };
            for entry in entries {
                let entry = entry.map_err(|e| Error::io(dir.display(), &e))?;
                let rel = directory.join(entry.file_name());
                if files.contains_key(&rel)
                    || !entry
                        .file_type()
                        .map_err(|e| Error::io(rel.display(), &e))?
                        .is_file()
                {
                    continue;
                }
                let path = entry.path();
                let (file, _) = crate::identity::open_regular_nonblock(&path)
                    .map_err(|e| Error::io(path.display(), &e))?;
                let mut head = Vec::with_capacity(8);
                file.take(8)
                    .read_to_end(&mut head)
                    .map_err(|e| Error::io(path.display(), &e))?;
                if atpkg::macho::is_macho_header(&head) {
                    files.insert(rel, digest(&path)?);
                }
            }
        }
        Ok(Self(files))
    }

    pub(super) fn contains(&self, root: &Path, path: &Path) -> bool {
        path.strip_prefix(root)
            .is_ok_and(|rel| self.0.contains_key(rel))
    }

    pub(super) fn check(&self, root: &Path) -> Result<()> {
        if !self.0.is_empty()
            && has_paired_linker(root)? != self.0.contains_key(Path::new(ROOT_LINKER))
        {
            return Err(Error::refused(format!(
                "packaging changed the presence of native Trust linkers {ROOT_LINKER} and {HOST_LINKER}"
            )));
        }
        for (rel, admitted) in &self.0 {
            let path = root.join(rel);
            if rel.starts_with("bin") && !atpkg::toolchain::is_executable_file(&path) {
                return Err(Error::refused(format!(
                    "admitted native Trust runtime at {} must remain executable",
                    rel.display()
                )));
            }
            if digest(&path)? != *admitted {
                return Err(Error::refused(format!(
                    "packaging changed the admitted native Trust runtime at {}. The seal must ship a portable image with the producer's bytes",
                    rel.display()
                )));
            }
        }
        Ok(())
    }
}

/// Both linker paths are independent executable copies of one image in a
/// Darwin SDK. A legacy SDK can omit LLD altogether. Only bootstrap makes it.
fn has_paired_linker(root: &Path) -> Result<bool> {
    let selected = executable_helper(root, ROOT_LINKER)?;
    let host = executable_helper(root, HOST_LINKER)?;
    match (selected, host) {
        (None, None) => Ok(false),
        (Some(a), Some(b))
            if !super::same_meta_inode(&a, &b) && a.permissions() == b.permissions() =>
        {
            Ok(true)
        }
        _ => Err(Error::refused(format!(
            "native Trust linkers {ROOT_LINKER} and {HOST_LINKER} must both be present as distinct executable files with matching permissions, or both absent"
        ))),
    }
}

/// The optional Miri component installs its public Targo frontend and interpreter together.
pub(super) fn has_miri_pair(root: &Path) -> Result<bool> {
    let frontend = executable_helper(root, "bin/targo-miri")?;
    let interpreter = executable_helper(root, "bin/trust-miri")?;
    match (frontend, interpreter) {
        (None, None) => Ok(false),
        (Some(frontend), Some(interpreter)) if !super::same_meta_inode(&frontend, &interpreter) => {
            Ok(true)
        }
        _ => Err(Error::refused(
            "Trust Miri requires both bin/targo-miri and bin/trust-miri as distinct executables",
        )),
    }
}

fn executable_helper(root: &Path, rel: &str) -> Result<Option<std::fs::Metadata>> {
    let path = root.join(rel);
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(path.display(), &e)),
    };
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return Err(Error::refused(format!(
            "native Trust helper {rel} must be a regular executable"
        )));
    }
    let actual = path
        .canonicalize()
        .map_err(|e| Error::io(path.display(), &e))?;
    let root = root
        .canonicalize()
        .map_err(|e| Error::io(root.display(), &e))?;
    if actual != root.join(rel) {
        return Err(Error::refused(format!(
            "native Trust helper {rel} must be inside the SDK at its producer path"
        )));
    }
    Ok(Some(meta))
}

fn digest(path: &Path) -> Result<String> {
    atpkg::sha256_file(path).map_err(|e| Error::io(path.display(), &e))
}
