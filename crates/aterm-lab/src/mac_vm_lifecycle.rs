// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordered retirement and durable observations of VMs allocated by this process.
//! Decoded records never supply custody or permission to operate a VM.

use std::path::{Path, PathBuf};

use aterm_json::{Value, json};
use aterm_uds::spawnfd::BirthKey;

pub(super) struct Journal {
    path: PathBuf,
    value: Value,
}

impl Journal {
    pub(super) fn new(path: PathBuf, allocation: Value) -> Result<Self, String> {
        if path
            .try_exists()
            .map_err(|e| format!("{}: {e}", path.display()))?
        {
            return Err(format!("{} already exists", path.display()));
        }
        let journal = Self {
            path,
            value: json!({"schema":"aterm-owned-mac-vm-v1", "complete":false,
                "allocation":allocation, "events":[]}),
        };
        journal.save()?;
        Ok(journal)
    }

    fn save(&self) -> Result<(), String> {
        let bytes = aterm_json::to_vec(&self.value).map_err(|e| e.to_string())?;
        aterm_uds::atomic_file::write(&self.path, &bytes, 0o600)
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }

    pub(super) fn event(&mut self, action: &str, observation: Value) -> Result<(), String> {
        let events = self
            .value
            .as_object_mut()
            .and_then(|object| object.get_mut("events"))
            .and_then(Value::as_array_mut)
            .expect("journal events");
        let sequence = events.len();
        events.push(json!({"sequence":sequence, "action":action, "observation":observation}));
        self.save()
    }

    pub(super) fn complete(&mut self, disposition: &str) -> Result<(), String> {
        let object = self.value.as_object_mut().expect("journal object");
        object.insert("complete".into(), json!(true));
        object.insert("disposition".into(), json!(disposition));
        if let Err(error) = self.save() {
            let object = self.value.as_object_mut().expect("journal object");
            object.insert("complete".into(), json!(false));
            object.insert("publication_error".into(), json!(error));
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn value(&self) -> Value {
        self.value.clone()
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

/// Always observe the owner's checked cleanup, preserving an original red or real interrupt.
pub(crate) fn after_cleanup<T>(
    work: Result<T, crate::Fail>,
    cleanup: impl FnOnce() -> Result<(), crate::Fail>,
) -> Result<T, crate::Fail> {
    match (work, cleanup()) {
        (work, Ok(())) => work,
        (Ok(_), Err(error)) => Err(error),
        (Err(mut work), Err(cleanup)) => {
            if work.code != 1 && work.stopped.is_none() {
                work.code = cleanup.code;
                work.stopped = cleanup.stopped;
            }
            work.msg = Some(format!(
                "{}; owned cleanup: {}",
                work.msg.unwrap_or_else(|| format!("exit {}", work.code)),
                cleanup
                    .msg
                    .unwrap_or_else(|| format!("exit {}", cleanup.code))
            ));
            Err(work)
        }
    }
}

/// Shared inputs stay in place unless the owner explicitly confirmed retirement.
/// Keeping files is evidence retention, never permission to control a process from metadata.
pub(crate) struct Scratch {
    directory: Option<aterm_tempfile::TempDir>,
    record: PathBuf,
}

impl Scratch {
    pub(crate) fn new(
        directory: aterm_tempfile::TempDir,
        record: &Path,
    ) -> Result<Self, crate::Fail> {
        crate::util::create_private_dir_all(record)
            .map_err(|error| format!("{}: {error}", record.display()))?;
        Ok(Self {
            directory: Some(directory),
            record: record.to_path_buf(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        self.directory.as_ref().expect("owned VM scratch").path()
    }

    pub(crate) fn discard_after_finish(&mut self) {
        drop(self.directory.take());
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(directory) = self.directory.take() {
            let retained = directory.keep();
            let name = retained
                .file_name()
                .expect("scratch leaf")
                .to_string_lossy();
            let path = self.record.join(format!("retained-{name}.scratch.json"));
            let receipt = json!({"schema":"aterm-owned-mac-scratch-v1", "complete":false,
                "path":retained, "reason":"owner did not confirm VM retirement and evidence publication"});
            let result = aterm_json::to_vec(&receipt)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    aterm_uds::atomic_file::write(&path, &bytes, 0o600).map_err(|e| e.to_string())
                });
            if let Err(error) = result {
                eprintln!("mac: retained VM scratch {}: {error}", retained.display());
            }
        }
    }
}

pub(super) fn birth_key(key: &BirthKey) -> Value {
    json!({"attempt":key.nonce(), "owner":key.owner().token(),
        "host":key.host(), "boot":key.boot(), "namespace":key.namespace()})
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Stop,
    Join,
    Delete,
    Keep,
    ReleaseSlots,
}

impl Action {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Join => "owning-reap",
            Self::Delete => "delete",
            Self::Keep => "keep",
            Self::ReleaseSlots => "release-slots",
        }
    }
}

/// Every operation is performed against the same in-memory owned allocation. In particular,
/// a successful stop is not an owning reap, and neither alone permits deletion or slot release.
/// The injected operation is also the source-only test seam; it never decodes a custody record.
pub(super) fn retire_with(
    running: bool,
    keep: bool,
    mut operation: impl FnMut(Action) -> Result<(), String>,
) -> Result<(), String> {
    if running {
        operation(Action::Stop)?;
        operation(Action::Join)?;
    }
    operation(if keep { Action::Keep } else { Action::Delete })?;
    operation(Action::ReleaseSlots)
}

/// Wait on the original Child while its launcher and descendants make CPU or I/O progress.
/// `root` supplies observations, never reap or signal authority. Unread or incomparable
/// observations cannot prove a stall. A wait error or one whole window without progress
/// leaves the actual Child with its owner; only its own terminal status is a reap receipt.
pub(super) fn wait_for_reap(
    root: u32,
    window: std::time::Duration,
    try_wait: impl FnMut() -> std::io::Result<Option<std::process::ExitStatus>>,
) -> Result<std::process::ExitStatus, String> {
    let mut progress = aterm_uds::deadline::ProcessProgressProbe::default();
    wait_for_reap_with(
        window,
        try_wait,
        std::time::Instant::now,
        || progress.read(root),
        std::thread::sleep,
    )
}

fn wait_for_reap_with(
    window: std::time::Duration,
    mut try_wait: impl FnMut() -> std::io::Result<Option<std::process::ExitStatus>>,
    mut now: impl FnMut() -> std::time::Instant,
    mut read: impl FnMut() -> Option<u64>,
    mut pause: impl FnMut(std::time::Duration),
) -> Result<std::process::ExitStatus, String> {
    let mut progress = aterm_uds::deadline::ProgressWindow::new(window, now());
    loop {
        if let Some(status) =
            try_wait().map_err(|error| format!("Tart launcher's owning wait: {error}"))?
        {
            return Ok(status);
        }
        let left = progress.observe(now(), 0, &mut read);
        if left.is_zero() {
            // Completion can race the last progress read; only another owning wait may
            // establish it, never a process-table observation of exit or absence.
            return try_wait()
                .map_err(|error| format!("Tart launcher's owning wait: {error}"))?
                .ok_or_else(|| {
                    format!(
                        "Tart launcher made no CPU or I/O progress for {window:?} without an owning reap; hang detector expired"
                    )
                });
        }
        pause(left.min(std::time::Duration::from_millis(100)));
    }
}

#[cfg(test)]
#[path = "mac_vm_lifecycle_tests.rs"]
mod tests;
