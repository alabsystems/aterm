// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What release-input discovery learned about immutable Git objects, kept between calls.
//!
//! Discovery asks the same questions of the same commits on every call: which paths a commit
//! changed, what a landing's judged delta was, whether a composed receipt's merge holds. An
//! object ID fixes each answer, so a call that finds the answer kept asks nothing. A fact is
//! named by everything its answer depends on: the question's own words (a Git read's whole
//! argument list; a rule's name with the source text that decides it) and the object IDs it
//! asks about, under the reader that answered ([`Facts::read_by`]: the Git that was asked). A
//! question that names a ref is never kept, because a ref moves; a failed or refused question
//! is never kept either, so only an answer Git gave stands for a later call.

use std::cell::Cell;
use std::path::{Path, PathBuf};

const DOMAIN: &[u8] = b"aterm-release-input-fact-v1";
const HEADER: &str = "aterm-release-input-fact-v1 ";
/// A window's facts no call has opened for this long are removed with their folder.
const IDLE: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);
const USED: &str = "used";
/// A kept answer larger than this is asked again instead of read back.
const MAX_FACT_BYTES: u64 = 64 * 1024 * 1024;

/// One release window's kept answers, and how many questions this value answered each way.
pub struct Facts {
    dir: Option<PathBuf>,
    reader: String,
    asked: Cell<usize>,
    kept: Cell<usize>,
}

impl Facts {
    /// Keep nothing: every question is asked.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            dir: None,
            reader: String::new(),
            asked: Cell::new(0),
            kept: Cell::new(0),
        }
    }

    /// The answers kept in `dir`, which the first kept answer creates.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
            ..Self::none()
        }
    }

    /// The answers of the release window that starts after `base`, kept in the release
    /// directory of the repository `root` is a checkout of, so every worktree, a dry run and
    /// the run after it read the same ones. Opening a window marks it used and removes the
    /// windows nothing opened for thirty days. Where `base` is no object ID nothing is kept.
    #[must_use]
    pub fn of(root: &Path, base: &str) -> Self {
        if !aterm_types::land_trailer::object_id(base) {
            return Self::none();
        }
        let windows = crate::engine::release_dir(root)
            .join(aterm_tempfile::QUIET)
            .join("release-input-facts.noindex");
        let dir = windows.join(base);
        if std::fs::create_dir_all(&dir).is_err() || std::fs::write(dir.join(USED), b"").is_err() {
            return Self::none();
        }
        retire_idle(&windows, &dir);
        Self::at(dir)
    }

    /// These facts as `reader` answers them (Git's own version line): an answer another
    /// reader kept is not this one's, so a changed Git asks every question again.
    #[must_use]
    pub fn read_by(mut self, reader: &str) -> Self {
        reader.clone_into(&mut self.reader);
        self
    }

    /// The answer to `question`: the kept one where every name in `objects` is an object ID
    /// and an answer was kept under exactly these words, else what `ask` says, kept when it
    /// answered. `question` holds every word the answer depends on, the objects included.
    ///
    /// # Errors
    /// `ask`'s own error, which is never kept. A store that cannot be read or written is no
    /// error: the question is asked.
    pub fn answer(
        &self,
        question: &[&[u8]],
        objects: &[&str],
        ask: impl FnOnce() -> Result<String, String>,
    ) -> Result<String, String> {
        let at = self
            .dir
            .as_ref()
            .filter(|_| {
                objects
                    .iter()
                    .all(|id| aterm_types::land_trailer::object_id(id))
            })
            .map(|dir| dir.join(name(&self.reader, question)));
        if let Some(at) = &at
            && let Some(kept) = read(at)
        {
            self.kept.set(self.kept.get() + 1);
            return Ok(kept);
        }
        self.asked.set(self.asked.get() + 1);
        let answer = ask()?;
        if let Some(at) = &at {
            keep(at, &answer);
        }
        Ok(answer)
    }

    /// How many questions were asked, and how many answered from the kept ones.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        (self.asked.get(), self.kept.get())
    }
}

fn digest(parts: &[&[u8]]) -> String {
    let mut hash = aterm_digest::Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    aterm_codec::hex::encode_lower(&hash.finalize())
}

fn name(reader: &str, question: &[&[u8]]) -> String {
    let mut parts = vec![DOMAIN, reader.as_bytes()];
    parts.extend_from_slice(question);
    digest(&parts)
}

/// The answer kept at `at`, where the file is whole: its first line names the digest of the
/// bytes after it. A cut or foreign file is no answer.
fn read(at: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(at).ok()?;
    if !meta.is_file() || meta.len() > MAX_FACT_BYTES {
        return None;
    }
    let text = String::from_utf8(std::fs::read(at).ok()?).ok()?;
    let (head, answer) = text.split_once('\n')?;
    (head.strip_prefix(HEADER)? == digest(&[answer.as_bytes()])).then(|| answer.to_owned())
}

/// Keep `answer` at `at` under a name no reader sees until the file is whole.
fn keep(at: &Path, answer: &str) {
    let Some(dir) = at.parent() else { return };
    let Some(file) = at.file_name() else { return };
    let mut partial = std::ffi::OsString::from(".");
    partial.push(file);
    partial.push(format!(".{}", std::process::id()));
    let partial = dir.join(partial);
    let text = format!("{HEADER}{}\n{answer}", digest(&[answer.as_bytes()]));
    if std::fs::create_dir_all(dir).is_err()
        || std::fs::write(&partial, text).is_err()
        || std::fs::rename(&partial, at).is_err()
    {
        let _ = std::fs::remove_file(&partial);
    }
}

/// Remove each window beside `current` whose use mark is older than [`IDLE`]. A window with
/// no readable mark is left: nothing says how old it is.
fn retire_idle(windows: &Path, current: &Path) {
    let Ok(entries) = std::fs::read_dir(windows) else {
        return;
    };
    for entry in entries.flatten() {
        let window = entry.path();
        if window == current {
            continue;
        }
        let idle = std::fs::symlink_metadata(window.join(USED))
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|used| used.elapsed().ok());
        if idle.is_some_and(|idle| idle > IDLE) {
            let _ = std::fs::remove_dir_all(&window);
        }
    }
}

#[cfg(test)]
#[path = "release_input_facts_tests.rs"]
mod tests;
