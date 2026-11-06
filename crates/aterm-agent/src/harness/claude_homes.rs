// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude homes Claude Code launches in aterm tabs serve from
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 Q4b). `aterm harness tap --launch`, which the
//! managed `claude` twin runs before every launch in a tab, notes the home the launch's
//! environment names before the run serves anything. So the node service's metrics publisher
//! folds that home's transcripts even when the run started and ended between two of its passes,
//! and after a restart, without looking in every folder on disk for a home.
//!
//! One file per home under `<harness state>/claude-homes/`, named by the FNV-1a/64 of the
//! home's path and holding `<unix secs>\n<path>`. Launches of one home write the same name,
//! each whole (an atomic replace), so no note needs a lock. A note is advisory and takes no
//! flush: after a power loss one may not read, and a read skips it. A read removes a note older
//! than [`KEEP_S`], and one whose folder is gone.
//!
//! The launch's own default home (`$HOME/.claude`) is never noted: the notes live in the harness
//! state of the launch's `HOME`, and the publisher that reads them, running under that `HOME`,
//! lists it. Nor is a home the disk census never looks into
//! ([`super::disk::discover::looked_at`]), where a read could raise a privacy prompt, nor a
//! launch's home with no `HOME` to judge that by; a reader judges each noted home again by its
//! own.

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The notes' folder under the harness state.
pub const DIR: &str = "claude-homes";

/// How long a note is kept: the transcripts' horizon (`spend::HORIZON_S`) and a day.
pub const KEEP_S: i64 = super::spend::HORIZON_S + 86_400;

/// The most notes one read takes, newest first.
pub const NOTES_MAX: usize = 256;

/// The most folder entries one read looks at.
const LIST_MAX: usize = 4096;

/// The most bytes one note holds.
const NOTE_MAX: u64 = 4096;

/// The home a launch serves from, where a note keeps it: its environment's `CLAUDE_CONFIG_DIR`
/// and `HOME` (`vars`), a relative one resolved against the launch's `cwd`, as Claude Code
/// resolves it. `None` for the default home of the launch's own `HOME` (`user_home`), for a home
/// the census never looks into, with no `user_home` to judge that by, and where none can be
/// named.
#[must_use]
pub fn launched(
    vars: (Option<&str>, Option<&str>),
    cwd: Option<&Path>,
    user_home: Option<&Path>,
) -> Option<PathBuf> {
    let user_home = user_home?;
    let (config, home) = vars;
    let dir = super::footer::claude_dir_of(config, home, || cwd.map(Path::to_path_buf))?;
    let default = super::footer::claude_dir_of(None, user_home.to_str(), || None);
    (dir.is_absolute()
        && default.as_ref() != Some(&dir)
        && super::disk::discover::looked_at(&dir, Some(user_home)))
    .then_some(dir)
}

/// Note at `now` that a launch serves from `home`, under `harness_state`: a whole replace with
/// no flush, since the note is advisory.
///
/// # Errors
/// The folder cannot be made private or the note written; a path that is not UTF-8 is not
/// noted.
pub fn note(harness_state: &Path, home: &Path, now: i64) -> std::io::Result<()> {
    let Some(path) = home.to_str() else {
        return Ok(());
    };
    let dir = harness_state.join(DIR);
    aterm_uds::private_file::ensure_private_dir(&dir)?;
    aterm_uds::atomic_file::write_flushed(
        &dir.join(name_of(path)),
        format!("{now}\n{path}").as_bytes(),
        0o600,
        None,
    )
}

/// Every home noted under `harness_state` within [`KEEP_S`] of `now` whose folder is there, with
/// when it was last launched, newest first and at most [`NOTES_MAX`]; an older note, and one
/// whose folder is gone, is removed. A note that does not read is skipped.
#[must_use]
pub fn read(harness_state: &Path, now: i64) -> Vec<(PathBuf, i64)> {
    let dir = harness_state.join(DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let floor = now.saturating_sub(KEEP_S);
    let mut out = Vec::new();
    for entry in entries.take(LIST_MAX).filter_map(Result::ok) {
        let file = entry.path();
        let Some((at, home)) = read_note(&file) else {
            continue;
        };
        if at < floor || !home.is_dir() {
            let _ = std::fs::remove_file(&file);
            continue;
        }
        out.push((home, at));
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(NOTES_MAX);
    out
}

/// A note's name: the FNV-1a/64 of the home's path, in hex.
fn name_of(path: &str) -> String {
    let hash =
        astream_wire::hash::fnv1a_64_update(astream_wire::hash::FNV1A_64_INIT, path.as_bytes());
    format!("{hash:016x}")
}

/// `(when, home)` from the note at `file`, where it reads and its name is its home's.
fn read_note(file: &Path) -> Option<(i64, PathBuf)> {
    let name = file.file_name()?.to_str()?;
    let mut text = String::new();
    std::fs::File::open(file)
        .ok()?
        .take(NOTE_MAX)
        .read_to_string(&mut text)
        .ok()?;
    let (at, path) = text.split_once('\n')?;
    let home = Path::new(path);
    if !home.is_absolute() || name_of(path) != name {
        return None;
    }
    Some((at.parse().ok()?, home.to_path_buf()))
}

#[cfg(test)]
#[path = "claude_homes_tests.rs"]
mod tests;
