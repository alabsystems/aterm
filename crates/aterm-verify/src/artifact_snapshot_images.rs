// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stable private driver images. Published namespaces are never renamed or removed, including
//! after an unsuccessful spawn: descendants may still execute their own `current_exe`.

use crate::compiler_artifact::Artifact;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

#[path = "artifact_snapshot_macho.rs"]
mod macho;

#[derive(Debug)]
pub(crate) struct Images {
    directory: PathBuf,
    files: Vec<(&'static str, Artifact)>,
}

impl Images {
    pub(crate) fn prepare(
        paths: &[(&'static str, PathBuf)],
        expected: &std::collections::BTreeMap<&'static str, String>,
    ) -> Result<Self, String> {
        let parent = aterm_tempfile::quiet_in(Path::new("/tmp")).join(format!(
            "aterm-verify-images-{}.noindex",
            aterm_uds::peer::our_uid()
        ));
        Self::prepare_in(&parent, paths, expected)
    }

    fn prepare_in(
        parent: &Path,
        paths: &[(&'static str, PathBuf)],
        expected: &std::collections::BTreeMap<&'static str, String>,
    ) -> Result<Self, String> {
        if paths.is_empty() || paths.len() > 8 || expected.len() != paths.len() {
            return Err("invalid AppKit image count".into());
        }
        let mut original = Vec::with_capacity(paths.len());
        let mut seen = std::collections::BTreeSet::new();
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm-appkit-private-images-v1\0");
        for (name, path) in paths {
            if Path::new(name).file_name().and_then(|n| n.to_str()) != Some(*name)
                || !seen.insert(*name)
                || path.file_name().and_then(|n| n.to_str()) != Some(*name)
            {
                return Err("AppKit image name is not its unique original basename".into());
            }
            let artifact = Artifact::open_bounded(path, 1 << 30)?;
            if expected.get(name).map(String::as_str) != Some(artifact.sha256().as_str()) {
                return Err(
                    "AppKit output differs from the compiler's original executable digest".into(),
                );
            }
            if artifact.len() > 1 << 30 {
                return Err("AppKit image exceeds 1 GiB".into());
            }
            macho::relocatable(artifact.file())?;
            artifact.validate()?;
            hash.update(&(name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
            hash.update(artifact.sha256().as_bytes());
            original.push((*name, artifact));
        }
        let key = aterm_codec::hex::encode_lower(&hash.finalize());
        aterm_uds::private_file::create_private_dir(parent).map_err(|e| e.to_string())?;
        let parent = parent.canonicalize().map_err(|e| e.to_string())?;
        // The existing per-owner lock counts the holder's progress, not a wall-clock budget.
        // Its namespace here is separate from the window admission/runtime directory.
        let _publication = crate::stages::take_window_lock(
            &parent,
            std::time::Duration::from_secs(60),
            crate::stages::holder_progress(),
        )?;
        let content = parent.join(&key);
        aterm_uds::private_file::create_private_dir(&content).map_err(|e| e.to_string())?;
        let marker = content.join("complete");
        match std::fs::symlink_metadata(&marker) {
            Ok(_) => {
                let record = aterm_uds::private_file::read_private(&content, "complete", 256)
                    .map_err(|e| e.to_string())?;
                let record = std::str::from_utf8(&record).map_err(|e| e.to_string())?;
                let name = record
                    .strip_prefix("v1 ")
                    .and_then(|s| s.strip_suffix('\n'))
                    .ok_or("malformed AppKit image completion record")?;
                if !name.starts_with("image-")
                    || name.len() > 128
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                    || Path::new(name).file_name().and_then(|s| s.to_str()) != Some(name)
                {
                    return Err("noncanonical AppKit image completion record".into());
                }
                // A prior publication may have left this record after its directory flush
                // failed. Reestablish that boundary before admitting the completed images.
                aterm_durable::Flush::Durable
                    .dir(&content)
                    .map_err(|e| e.to_string())?;
                return Self::open(content.join(name), &original);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        // Allocate one fresh final namespace, with bounded collision handling in TempDir.
        // Keep it BEFORE the first image byte is written. Past incomplete candidates are
        // neither scanned nor counted against a lifetime cap, and are never changed.
        let directory = aterm_tempfile::Builder::new()
            .prefix("image-")
            .suffix(".noindex")
            .tempdir_in(&content)
            .map_err(|e| e.to_string())?
            .keep();
        for (name, artifact) in &original {
            artifact.copy_held(&directory.join(name))?;
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500))
            .map_err(|e| e.to_string())?;
        // A durable completion must never get ahead of the copied bytes, permissions,
        // or the image entries it names.
        aterm_durable::Flush::Durable
            .dir(&directory)
            .map_err(|e| e.to_string())?;
        let name = directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("AppKit image namespace name")?;
        let record = format!("v1 {name}\n");
        let images = Self::open(directory.clone(), &original)?;
        // Only this small metadata file is atomically renamed; images and namespaces never
        // move. NOREPLACE refuses a conflicting or corrupt existing completion record.
        let (temporary, mut file) = aterm_uds::atomic_file::create_temp_beside(&marker, 0o600)
            .map_err(|e| e.to_string())?;
        let published = (|| {
            file.write_all(record.as_bytes())
                .map_err(|e| e.to_string())?;
            file.set_permissions(std::fs::Permissions::from_mode(0o400))
                .map_err(|e| e.to_string())?;
            aterm_durable::Flush::Durable
                .file(&file)
                .map_err(|e| e.to_string())?;
            aterm_dirfd::renameat_with(
                aterm_dirfd::CWD,
                temporary.as_path(),
                aterm_dirfd::CWD,
                marker.as_path(),
                aterm_dirfd::RenameFlags::NOREPLACE,
            )
            .map_err(|e| e.to_string())?;
            aterm_durable::Flush::Durable
                .dir(&content)
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })();
        // A leftover here is metadata only, never an executable.
        let _ = std::fs::remove_file(&temporary);
        published?;
        Ok(images)
    }

    fn open(directory: PathBuf, original: &[(&'static str, Artifact)]) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(&directory).map_err(|e| e.to_string())?;
        if !metadata.is_dir()
            || metadata.uid() != aterm_uds::peer::our_uid()
            || metadata.mode() & 0o7777 != 0o500
        {
            return Err("completed AppKit image directory is not sealed and private".into());
        }
        let mut files = Vec::with_capacity(original.len());
        for (name, source) in original {
            let private = Artifact::open_bounded(&directory.join(name), 1 << 30)?;
            private.sealed_private()?;
            if private.sha256() != source.sha256() || private.len() != source.len() {
                return Err("completed AppKit private image differs from the build".into());
            }
            files.push((*name, private));
        }
        let images = Self { directory, files };
        images.validate()?;
        Ok(images)
    }

    pub(crate) fn paths(&self) -> Vec<(&'static str, PathBuf)> {
        self.files
            .iter()
            .map(|(name, image)| (*name, image.selected.clone()))
            .collect()
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.validate_namespace()?;
        for (_, image) in &self.files {
            image.validate()?;
        }
        Ok(())
    }

    pub(crate) fn validate_program(&self, path: &Path) -> Result<(), String> {
        self.validate_namespace()?;
        self.files
            .iter()
            .find(|(_, image)| image.selected == path)
            .ok_or("AppKit command is not its prepared image")?
            .1
            .validate()
    }

    fn validate_namespace(&self) -> Result<(), String> {
        let metadata = std::fs::symlink_metadata(&self.directory).map_err(|e| e.to_string())?;
        if !metadata.is_dir()
            || metadata.uid() != aterm_uds::peer::our_uid()
            || metadata.mode() & 0o7777 != 0o500
        {
            return Err("private AppKit image namespace changed".into());
        }
        let names = std::fs::read_dir(&self.directory)
            .map_err(|e| e.to_string())?
            .take(self.files.len() + 1)
            .map(|e| e.map(|e| e.file_name()))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map_err(|e| e.to_string())?;
        if names.len() != self.files.len()
            || self
                .files
                .iter()
                .any(|(name, _)| !names.contains(std::ffi::OsStr::new(name)))
        {
            return Err("private AppKit image membership changed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "artifact_snapshot_images_tests.rs"]
mod tests;
