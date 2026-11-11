// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What keeps the mug from keeping this Mac awake (`<state dir>/acaffeine.trouble`): `since=<unix
//! seconds>` and `why=<one line>`. A `serve` that will be started again and a mug whose activity
//! was refused write it ([`note`]), keeping the first `since` while the trouble lasts; a mug that
//! holds its activity, or a person's off, removes it ([`clear`]). The window's keep-awake watch
//! raises it as an exceptions entry once it has lasted [`LASTING_SECS`], and `aterm doctor`'s `awake:`
//! row says it.

use std::io;
use std::path::Path;

use crate::paths::Paths;

/// How long a trouble lasts before the window raises it ([`Trouble::lasting`]): long enough for
/// launchd's retries to clear a passing one (a full disk freed, an update landing).
pub const LASTING_SECS: u64 = 10 * 60;

/// The most bytes [`read`] reads.
const READ_CAP: u64 = 4096;

/// The longest `why` kept, in characters.
const WHY_CAP: usize = 400;

/// One trouble, as the file keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trouble {
    /// When it was first seen, in Unix seconds.
    pub since: u64,
    /// What it is, in one line.
    pub why: String,
}

impl Trouble {
    /// Whether it has lasted [`LASTING_SECS`] at `now` (Unix seconds); a clock now behind `since`
    /// reads as not yet.
    #[must_use]
    pub const fn lasting(&self, now: u64) -> bool {
        now.saturating_sub(self.since) >= LASTING_SECS
    }
}

fn clean_why(raw: &str) -> String {
    let why: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(WHY_CAP)
        .collect();
    why.trim().to_string()
}

/// The file's text for `trouble`: `why` made one printable line of at most [`WHY_CAP`]
/// characters.
#[must_use]
pub fn encode(trouble: &Trouble) -> String {
    format!("since={}\nwhy={}\n", trouble.since, clean_why(&trouble.why))
}

/// The trouble `text` keeps, `None` unless it names both keys.
#[must_use]
pub fn decode(text: &str) -> Option<Trouble> {
    let (mut since, mut why) = (None, None);
    for line in text.lines() {
        match line.split_once('=') {
            Some(("since", value)) => {
                since = (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| value.parse().ok())
                    .flatten();
            }
            Some(("why", value)) => why = Some(clean_why(value)),
            _ => {}
        }
    }
    Some(Trouble {
        since: since?,
        why: why?,
    })
}

/// The trouble recorded under `paths`, if any.
#[must_use]
pub fn read(paths: &Paths) -> Option<Trouble> {
    read_at(&paths.trouble_file())
}

fn read_at(path: &Path) -> Option<Trouble> {
    use std::io::Read as _;
    if !crate::paths::parent_is_real_dir(path) {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > READ_CAP {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(READ_CAP + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > READ_CAP {
        return None;
    }
    decode(&text)
}

/// Record `why` at `now` (Unix seconds), keeping the `since` of a trouble already recorded.
///
/// # Errors
///
/// The state directory cannot be made, or the file cannot be written.
pub fn note_at(paths: &Paths, why: &str, now: u64) -> io::Result<()> {
    let path = paths.trouble_file();
    let since = read_at(&path).map_or(now, |was| was.since.min(now));
    let text = encode(&Trouble {
        since,
        why: why.to_string(),
    });
    crate::paths::ensure_real_dir(&paths.state_dir)?;
    #[cfg(unix)]
    {
        aterm_uds::atomic_file::write_checked(&path, text.as_bytes(), 0o600, || {
            if crate::paths::existing_real_dir(&paths.state_dir)? {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} does not exist", paths.state_dir.display()),
                ))
            }
        })
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, text)
    }
}

/// [`note_at`] now; a failure is logged, since the trouble it would record is logged beside it.
pub fn note(paths: &Paths, why: &str) {
    let now = aterm_time::unix_secs().unwrap_or_default();
    if let Err(e) = note_at(paths, why, now) {
        aterm_log::warn!(
            "acaffeine: could not record {}: {e}",
            paths.trouble_file().display()
        );
    }
}

/// Remove the record: the mug holds its activity, or the person turned keep-awake off.
pub fn clear(paths: &Paths) {
    match crate::paths::existing_real_dir(&paths.state_dir) {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            aterm_log::warn!(
                "acaffeine: could not remove {}: {e}",
                paths.trouble_file().display()
            );
            return;
        }
    }
    match std::fs::remove_file(paths.trouble_file()) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => aterm_log::warn!(
            "acaffeine: could not remove {}: {e}",
            paths.trouble_file().display()
        ),
        _ => {}
    }
}

#[cfg(test)]
#[path = "trouble_tests.rs"]
mod tests;
