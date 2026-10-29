// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Temporary live-owner pause for an explicitly requested development run.
//! Stop the private Runtime before restoring or dropping this guard. The config
//! write lock serializes with Settings saves; writers ignoring that lock are
//! outside this transaction's protection.

use aterm_toml::edit::{DocumentMut, Item, Table, Value};
use aterm_uds::{atomic_file, private_file};
use std::{
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_CONFIG: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored,
    AlreadyOriginal,
}

enum State {
    Prepared,
    Paused,
    Restored,
    RestoreNeeded(String),
}

pub struct PauseGuard {
    live: PathBuf,
    original: Vec<u8>,
    paused: Vec<u8>,
    mode: u32,
    private: PathBuf,
    backup: PathBuf,
    scratch: Option<aterm_tempfile::TempDir>,
    stop: Arc<AtomicBool>,
    state: State,
}

impl PauseGuard {
    /// Prepare an exact private configuration and backup, then pause only live
    /// Commons discovery under the same write lock Settings uses. An existing,
    /// safely owned configuration with Commons enabled is required.
    pub fn pause(path: &Path) -> io::Result<Self> {
        Self::pause_with_installer(path, |stop| {
            aterm_uds::process::take_stop_signals("commons-probe-stop", move |_| {
                stop.store(true, Ordering::Release);
            })
        })
    }

    fn pause_with_installer(
        path: &Path,
        install: impl FnOnce(Arc<AtomicBool>) -> bool,
    ) -> io::Result<Self> {
        let (original, mode) = read_config(path)?;
        let paused = paused_config(&original)?;
        let stop = Arc::new(AtomicBool::new(false));
        // No scratch or live state is written before stop handling is installed.
        if !install(stop.clone()) {
            return Err(io::Error::other(
                "stop signal handling is unavailable; live config was not changed",
            ));
        }
        interrupted(&stop)?;
        let scratch = aterm_tempfile::Builder::new()
            .prefix("cpl-")
            .tempdir_in("/tmp")?;
        let private = scratch.path().join("config.toml");
        let backup = scratch.path().join("original.toml");
        atomic_file::write(&private, &original, 0o600)?;
        atomic_file::write(&backup, &original, 0o600)?;
        let mut guard = Self {
            live: path.to_owned(),
            original,
            paused,
            mode,
            private,
            backup,
            scratch: Some(scratch),
            stop,
            state: State::Prepared,
        };
        let source = atomic_file::ConfigFile::open(path)?;
        let lock = source.lock()?;
        check_current(path, &guard.original, mode)?;
        interrupted(&guard.stop)?;
        // A post-rename directory-barrier error is an uncertain result. Arm
        // restoration before attempting publication and release the write lock
        // before any error can drop the guard and attempt a restore.
        guard.state = State::Paused;
        let written = atomic_file::write_checked(path, &guard.paused, mode, || {
            interrupted(&guard.stop)?;
            check_current(path, &guard.original, mode)
        });
        drop(lock);
        written?;
        Ok(guard)
    }

    pub fn private_config(&self) -> &Path {
        &self.private
    }

    pub fn backup_path(&self) -> &Path {
        &self.backup
    }

    pub fn stop_requested(&self) -> bool {
        // A concurrent Settings save is a current policy decision, even though
        // the private Runtime still has the captured original config. Stop it
        // before restoration rather than continue under that older policy.
        self.stop.load(Ordering::Acquire)
            || check_current(&self.live, &self.paused, self.mode).is_err()
    }

    /// Restore exact original bytes and mode only while the live file still
    /// equals this guard's paused version. A concurrent edit is preserved and
    /// the private backup is retained for explicit recovery.
    pub fn restore(&mut self) -> io::Result<RestoreOutcome> {
        match &self.state {
            State::Prepared | State::Restored => return Ok(RestoreOutcome::AlreadyOriginal),
            State::RestoreNeeded(detail) => return Err(io::Error::other(detail.clone())),
            State::Paused => {}
        }
        let result = self.restore_locked();
        match result {
            Ok(outcome) => {
                self.state = State::Restored;
                Ok(outcome)
            }
            Err(error) => {
                // The TempDir must not remove the only exact backup when the
                // guard is dropped after a conflict or uncertain write.
                if let Some(scratch) = self.scratch.take() {
                    let _retained = scratch.keep();
                }
                let detail = format!(
                    "restore-needed: {}; preserved original config at {}: {error}",
                    self.live.display(),
                    self.backup.display(),
                );
                self.state = State::RestoreNeeded(detail.clone());
                Err(io::Error::other(detail))
            }
        }
    }

    fn restore_locked(&self) -> io::Result<RestoreOutcome> {
        let source = atomic_file::ConfigFile::open(&self.live)?;
        let _lock = source.lock()?;
        let (current, mode) = read_config(&self.live)?;
        if current == self.original && mode == self.mode {
            return Ok(RestoreOutcome::AlreadyOriginal);
        }
        if current != self.paused || mode != self.mode {
            return Err(io::Error::other(
                "live configuration changed; refusing to overwrite it",
            ));
        }
        atomic_file::write_checked(&self.live, &self.original, self.mode, || {
            check_current(&self.live, &self.paused, self.mode)
        })?;
        Ok(RestoreOutcome::Restored)
    }
}

impl Drop for PauseGuard {
    fn drop(&mut self) {
        if matches!(self.state, State::Paused)
            && let Err(error) = self.restore()
        {
            eprintln!("{error}");
        }
    }
}

fn interrupted(stop: &AtomicBool) -> io::Result<()> {
    if stop.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "stop requested before live pause publication",
        ))
    } else {
        Ok(())
    }
}

fn read_config(path: &Path) -> io::Result<(Vec<u8>, u32)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| io::Error::other("configuration must have an explicit parent directory"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("configuration name is not valid UTF-8"))?;
    let mut bytes = vec![0; MAX_CONFIG];
    let read =
        private_file::read_owned_into(parent, name, MAX_CONFIG, &mut bytes).map_err(|error| {
            io::Error::other(format!("configuration is not a safe owned file: {error}"))
        })?;
    let metadata = std::fs::symlink_metadata(path)?;
    if (metadata.dev(), metadata.ino()) != (read.dev, read.ino) {
        return Err(io::Error::other("configuration changed during owned read"));
    }
    bytes.truncate(read.len);
    Ok((bytes, metadata.permissions().mode() & 0o777))
}

fn check_current(path: &Path, expected: &[u8], expected_mode: u32) -> io::Result<()> {
    let (current, mode) = read_config(path)?;
    if current != expected || mode != expected_mode {
        return Err(io::Error::other(
            "configuration changed during the locked edit",
        ));
    }
    Ok(())
}

fn paused_config(original: &[u8]) -> io::Result<Vec<u8>> {
    let text = std::str::from_utf8(original)
        .map_err(|_| io::Error::other("configuration is not valid UTF-8"))?;
    let mut document: DocumentMut = text
        .parse()
        .map_err(|_| io::Error::other("configuration is not valid TOML"))?;
    if document.get("commons").is_none() {
        document
            .as_table_mut()
            .insert("commons", Item::Table(Table::new()));
    }
    let commons = document
        .as_table_mut()
        .get_mut("commons")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| io::Error::other("Commons configuration is not a table"))?;
    match commons.get_mut("enabled") {
        Some(item) => {
            if item.as_bool() != Some(true) {
                return Err(io::Error::other(
                    "Commons must already be enabled; refusing a policy bypass",
                ));
            }
            let decor = item.as_value().expect("a boolean value").decor().clone();
            let mut value = Value::from(false);
            *value.decor_mut() = decor;
            *item = Item::Value(value);
        }
        None => {
            commons.insert("enabled", Item::Value(Value::from(false)));
        }
    }
    Ok(document.to_string().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &[u8] = b"# keep this comment\nfont_px = 14\n\n[commons]\nenabled = true # owner setting\nexclude_tables = [] # exact policy\n";

    fn fixture(bytes: &[u8]) -> (aterm_tempfile::TempDir, PathBuf) {
        let root = aterm_tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = root.path().join("aterm.toml");
        atomic_file::write(&path, bytes, 0o644).unwrap();
        (root, path)
    }

    #[test]
    fn pause_copies_exact_policy_preserves_modes_and_drop_restores_original() {
        let (root, path) = fixture(ORIGINAL);
        let mut signal = None;
        let guard = PauseGuard::pause_with_installer(&path, |stop| {
            signal = Some(stop);
            true
        })
        .unwrap();
        let private = guard.private_config().to_owned();
        assert_eq!(std::fs::read(&private).unwrap(), ORIGINAL);
        assert_eq!(std::fs::read(guard.backup_path()).unwrap(), ORIGINAL);
        let paused = std::fs::read_to_string(&path).unwrap();
        assert!(paused.contains("enabled = false # owner setting"));
        assert!(paused.contains("exclude_tables = [] # exact policy"));
        assert!(paused.starts_with("# keep this comment\nfont_px = 14\n"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(
            std::fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
        signal.unwrap().store(true, Ordering::Release);
        assert!(guard.stop_requested());
        drop(guard);
        assert_eq!(std::fs::read(&path).unwrap(), ORIGINAL);
        assert!(!private.exists());
    }

    #[test]
    fn concurrent_config_edit_is_preserved_and_backup_survives_drop() {
        let (_root, path) = fixture(ORIGINAL);
        let mut guard = PauseGuard::pause_with_installer(&path, |_| true).unwrap();
        let backup = guard.backup_path().to_owned();
        let changed = b"# concurrent Settings save\nfont_px = 16\n[commons]\nenabled = false\n";
        let config = atomic_file::ConfigFile::open(&path).unwrap();
        let lock = config.lock().unwrap();
        atomic_file::write(&path, changed, 0o644).unwrap();
        drop(lock);
        assert!(
            guard.stop_requested(),
            "a Settings change must stop the private owner"
        );
        assert!(
            guard
                .restore()
                .unwrap_err()
                .to_string()
                .contains("restore-needed")
        );
        assert_eq!(std::fs::read(&path).unwrap(), changed);
        drop(guard);
        assert_eq!(std::fs::read(&path).unwrap(), changed);
        assert_eq!(std::fs::read(&backup).unwrap(), ORIGINAL);
        std::fs::remove_dir_all(backup.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_signal_handling_or_disabled_policy_never_changes_live_config() {
        let (_root, path) = fixture(ORIGINAL);
        assert!(PauseGuard::pause_with_installer(&path, |_| false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), ORIGINAL);
        let (_disabled_root, disabled) = fixture(b"[commons]\nenabled = false\n");
        assert!(
            PauseGuard::pause_with_installer(&disabled, |_| panic!(
                "disabled policy must refuse before signals"
            ))
            .is_err()
        );
        assert_eq!(
            std::fs::read(&disabled).unwrap(),
            b"[commons]\nenabled = false\n"
        );
    }
}
