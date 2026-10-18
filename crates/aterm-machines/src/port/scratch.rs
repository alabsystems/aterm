// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An allocation token, not a claim inferred from a path or its permissions.

use super::System;
use std::{
    io,
    path::{Path, PathBuf},
};

/// An exclusive ephemeral directory owned until cleanup or explicit transfer.
///
/// Keep the guard alive while an external tool or publisher uses its inner paths.
/// Cleanup is path based: this does not protect against a hostile concurrent ancestor
/// or same-UID replacement, and Drop is best effort rather than proof of cleanup.
pub struct Scratch<'s> {
    owner: Box<dyn OwnedDirectory + 's>,
}

impl<'s> Scratch<'s> {
    /// Allocate under the backend's temporary parent (the Fake stays in RAM).
    ///
    /// # Errors
    /// Invalid label, exhausted exclusive allocation attempts or backend refusal.
    pub fn new(sys: &'s dyn System, label: &str) -> io::Result<Self> {
        prefix(label)?;
        sys.scratch_dir(None, label)
    }

    /// Allocate in `parent`, keeping staged renames on its filesystem.
    /// The caller admits or creates that parent; this never adopts an occupied child.
    ///
    /// # Errors
    /// Invalid label, exhausted exclusive allocation attempts or backend refusal.
    pub fn new_in(sys: &'s dyn System, parent: &Path, label: &str) -> io::Result<Self> {
        prefix(label)?;
        sys.scratch_dir(Some(parent), label)
    }

    /// The exact allocation this guard owns.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.owner.path()
    }

    /// Remove this allocation. An error retains the token for retry or explicit keep.
    ///
    /// # Errors
    /// The cleanup error, with unresolved ownership still held by this guard.
    pub fn close(&mut self) -> io::Result<()> {
        self.owner.close()
    }

    /// Transfer cleanup responsibility, for example with a retained rollback bundle.
    #[must_use]
    pub fn keep(self) -> PathBuf {
        self.owner.keep()
    }

    pub(super) fn from_owner(owner: Box<dyn OwnedDirectory + 's>) -> Self {
        Self { owner }
    }
}

impl std::fmt::Debug for Scratch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scratch")
            .field("path", &self.path())
            .finish()
    }
}

pub(super) trait OwnedDirectory: Send + Sync {
    fn path(&self) -> &Path;
    fn close(&mut self) -> io::Result<()>;
    fn keep(self: Box<Self>) -> PathBuf;
}

pub(super) fn prefix(label: &str) -> io::Result<String> {
    if label.is_empty()
        || label.len() > 48
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scratch label is not a short filename component",
        ));
    }
    Ok(format!(".aterm-machines-{label}-"))
}

impl Scratch<'static> {
    pub(super) fn live(parent: Option<&Path>, label: &str) -> io::Result<Self> {
        let builder = aterm_tempfile::Builder::new().prefix(&prefix(label)?);
        let dir = match parent {
            Some(parent) => builder.tempdir_in(parent)?,
            None => builder.tempdir()?,
        };
        // The successfully created TempDir is immediately retained, including on unwind.
        Ok(Self::from_owner(Box::new(LiveDirectory { dir: Some(dir) })))
    }
}

struct LiveDirectory {
    dir: Option<aterm_tempfile::TempDir>,
}

impl OwnedDirectory for LiveDirectory {
    fn path(&self) -> &Path {
        self.dir
            .as_ref()
            .map_or_else(|| Path::new(""), aterm_tempfile::TempDir::path)
    }

    fn close(&mut self) -> io::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        match std::fs::remove_dir_all(dir.path()) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Disarm only after successful removal. A failed close retains the owner.
        if let Some(dir) = self.dir.take() {
            let _ = dir.keep();
        }
        Ok(())
    }

    fn keep(mut self: Box<Self>) -> PathBuf {
        self.dir
            .take()
            .map_or_else(PathBuf::new, aterm_tempfile::TempDir::keep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Live, System};

    #[test]
    fn occupied_scratch_candidates_are_preserved_and_only_the_allocation_is_removed() {
        let sys = Fake::root();
        let parent = Path::new("/cache");
        let file = Fake::scratch_path(Some(parent), "test", 0);
        let link = Fake::scratch_path(Some(parent), "test", 1);
        let subtree = Fake::scratch_path(Some(parent), "test", 2).join("private");
        sys.file(file.to_str().unwrap(), b"file owner");
        sys.link(link.to_str().unwrap(), "/elsewhere");
        sys.file(subtree.to_str().unwrap(), b"directory owner");
        let owned = Scratch::new_in(&sys, parent, "test").unwrap();
        let path = owned.path().to_owned();
        assert_eq!(path, Fake::scratch_path(Some(parent), "test", 3));
        let payload = path.join("payload");
        sys.write_file(&payload, b"this operation", 0o600).unwrap();
        assert_eq!(sys.read_file(&payload).unwrap(), b"this operation");
        drop(owned);
        assert_eq!(sys.stat(&path).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(sys.read_file(&payload).is_err());
        assert_eq!(sys.read_file(&file).unwrap(), b"file owner");
        assert_eq!(sys.read_link(&link).unwrap(), Path::new("/elsewhere"));
        assert_eq!(sys.read_file(&subtree).unwrap(), b"directory owner");
        // Exhaustion does not acquire cleanup rights over any occupied candidate.
        let sys = Fake::root();
        for n in 0..5 {
            let path = Fake::scratch_path(Some(parent), "full", n);
            sys.file(path.to_str().unwrap(), b"not ours");
        }
        assert_eq!(
            Scratch::new_in(&sys, parent, "full").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        for n in 0..5 {
            assert_eq!(
                sys.read_file(&Fake::scratch_path(Some(parent), "full", n))
                    .unwrap(),
                b"not ours"
            );
        }
    }

    #[test]
    fn failed_close_retains_custody_and_keep_survives_a_fresh_retry() {
        let sys = Fake::root();
        let mut owner = Scratch::new(&sys, "retry").unwrap();
        let first = owner.path().to_owned();
        sys.write_file(&first.join("old.app"), b"rollback bytes", 0o600)
            .unwrap();
        sys.scratch_close_fails(&first, Some(io::ErrorKind::PermissionDenied));
        assert_eq!(
            owner.close().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(owner.path(), first);
        assert_eq!(
            sys.read_file(&first.join("old.app")).unwrap(),
            b"rollback bytes"
        );
        let kept = owner.keep();
        assert_eq!(kept, first);
        let fresh = Scratch::new(&sys, "retry").unwrap();
        assert_ne!(fresh.path(), kept);
        drop(fresh);
        assert_eq!(
            sys.read_file(&kept.join("old.app")).unwrap(),
            b"rollback bytes"
        );
        // A separate failure may instead be retried successfully without transferring.
        let mut retry = Scratch::new(&sys, "retry").unwrap();
        let path = retry.path().to_owned();
        sys.scratch_close_fails(&path, Some(io::ErrorKind::PermissionDenied));
        assert!(retry.close().is_err());
        sys.scratch_close_fails(&path, None);
        retry.close().unwrap();
        retry.close().unwrap();
        assert!(sys.stat(&path).is_err());
        assert!(Scratch::new(&sys, "../outside").is_err());
        assert!(Scratch::new(&sys, "").is_err());
    }

    #[test]
    fn envelope_failure_cleans_its_allocation_without_claiming_an_old_directory() {
        let sys = Fake::root();
        let occupant = Fake::scratch_path(None, "seal", 0);
        sys.file(occupant.to_str().unwrap(), b"private occupant");
        sys.on(
            "/usr/bin/ssh-keygen -y -P '' -f /key",
            crate::port::Ran::ok("public key"),
        );
        let owned = Fake::scratch_path(None, "seal", 1);
        let body = owned.join(format!("aterm-envelope-{}-0.toml", std::process::id()));
        let cmd = crate::port::Cmd::new("/usr/bin/ssh-keygen")
            .args([
                "-Y",
                "sign",
                "-f",
                "/key",
                "-n",
                crate::protocol::SIG_NAMESPACE,
            ])
            .arg(&body);
        sys.on(
            &cmd.display(),
            crate::port::Ran::exit(17, "refused synthetic signing"),
        );
        let envelope = crate::envelope::Envelope {
            schema: crate::SCHEMA,
            host: "U".into(),
            host_name: "u".into(),
            serial: 1,
            issued: 1,
            expires: 2,
            action: crate::envelope::Action::Facts,
            disrupt: crate::envelope::Disrupt::WhenIdle,
            idle_secs: 900,
            baseline: None,
            artifacts: vec![],
            exec: None,
            upgrade: None,
            rekey: None,
        };
        let why = crate::envelope::seal(&sys, &envelope, Path::new("/key")).unwrap_err();
        assert!(why.contains("refused synthetic signing"), "{why}");
        assert!(sys.calls().iter().any(|call| call == &cmd.display()));
        assert!(sys.stat(&owned).is_err());
        assert_eq!(sys.read_file(&occupant).unwrap(), b"private occupant");
    }

    #[test]
    fn staged_command_input_uses_the_system_backend_and_ends_with_its_owner() {
        let sys = Fake::root();
        let staged = crate::port::staging_file(&sys, b"synthetic signed wire").unwrap();
        let path = staged.path().to_owned();
        assert_eq!(sys.read_file(&path).unwrap(), b"synthetic signed wire");
        assert_eq!(sys.stat(&path).unwrap().mode, 0o600);
        drop(staged);
        assert_eq!(sys.stat(&path).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn live_writers_leave_old_stage_names_and_failed_streams_untouched() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let dest = dir.path().join("file");
        let old_stage = dir
            .path()
            .join(format!(".file.aterm-machines-{}", std::process::id()));
        std::fs::write(&old_stage, b"private collision").unwrap();
        super::super::write_atomic(&dest, b"first", 0o600).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"first");
        let live = Live {
            agent_exe: PathBuf::from("/unused-by-this-file-fixture"),
        };
        let source = dir.path().join("source");
        std::fs::write(&source, b"copied").unwrap();
        live.copy_file(&source, &dest, 0o600).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"copied");
        struct Refuses;
        impl io::Read for Refuses {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("synthetic read refusal"))
            }
        }
        let mut stream = Refuses;
        assert!(live.write_stream(&dest, &mut stream, 0o600).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"copied");
        assert_eq!(std::fs::read(&old_stage).unwrap(), b"private collision");
        let mut names = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        let mut expected = vec![
            old_stage.file_name().unwrap().to_owned(),
            "file".into(),
            "source".into(),
        ];
        expected.sort();
        assert_eq!(
            names, expected,
            "failed/successful owned staging is cleaned"
        );
    }
}
