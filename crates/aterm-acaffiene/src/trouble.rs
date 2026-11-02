// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What keeps the mug from keeping this Mac awake (`<state dir>/acaffiene.trouble`): `since=<unix
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

/// The file's text for `trouble`: `why` made one printable line of at most [`WHY_CAP`]
/// characters.
#[must_use]
pub fn encode(trouble: &Trouble) -> String {
    let why: String = trouble
        .why
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(WHY_CAP)
        .collect();
    format!("since={}\nwhy={}\n", trouble.since, why.trim())
}

/// The trouble `text` keeps, `None` unless it names both keys.
#[must_use]
pub fn decode(text: &str) -> Option<Trouble> {
    let (mut since, mut why) = (None, None);
    for line in text.lines() {
        match line.split_once('=') {
            Some(("since", value)) => since = value.parse().ok(),
            Some(("why", value)) => why = Some(value.to_string()),
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
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(READ_CAP).read_to_string(&mut text).ok()?;
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
    std::fs::create_dir_all(&paths.state_dir)?;
    #[cfg(unix)]
    {
        aterm_uds::atomic_file::write(&path, text.as_bytes(), 0o600)
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
            "acaffiene: could not record {}: {e}",
            paths.trouble_file().display()
        );
    }
}

/// Remove the record: the mug holds its activity, or the person turned keep-awake off.
pub fn clear(paths: &Paths) {
    match std::fs::remove_file(paths.trouble_file()) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => aterm_log::warn!(
            "acaffiene: could not remove {}: {e}",
            paths.trouble_file().display()
        ),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(dir: &Path) -> Paths {
        Paths {
            state_dir: dir.join("state"),
            home: dir.join("home"),
            logs_dir: dir.join("logs"),
        }
    }

    /// A trouble keeps the time it was first seen while it is noted again, and a cleared one
    /// starts anew; it lasts once [`LASTING_SECS`] have passed since then.
    #[test]
    fn a_trouble_keeps_when_it_began_until_it_is_cleared() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let paths = scratch(dir.path());
        assert_eq!(read(&paths), None);
        note_at(&paths, "the disk is full", 1_000).expect("noted");
        note_at(&paths, "the disk is still full", 1_500).expect("noted again");
        let trouble = read(&paths).expect("recorded");
        assert_eq!(
            trouble,
            Trouble {
                since: 1_000,
                why: "the disk is still full".to_string()
            }
        );
        assert!(!trouble.lasting(1_000 + LASTING_SECS - 1));
        assert!(trouble.lasting(1_000 + LASTING_SECS));
        assert!(!trouble.lasting(0), "a clock set back reads as not yet");
        clear(&paths);
        assert_eq!(read(&paths), None);
        clear(&paths);
        note_at(&paths, "again", 9_000).expect("noted");
        assert_eq!(read(&paths).map(|t| t.since), Some(9_000));
    }

    /// The file is two lines, the reason one printable line however it was written.
    #[test]
    fn the_reason_is_one_printable_line() {
        let text = encode(&Trouble {
            since: 7,
            why: "launchctl said:\nno\u{1b}[2J".to_string(),
        });
        assert_eq!(text, "since=7\nwhy=launchctl said: no [2J\n");
        assert_eq!(
            decode(&text),
            Some(Trouble {
                since: 7,
                why: "launchctl said: no [2J".to_string()
            })
        );
        for bad in ["", "since=7\n", "why=x\n", "since=x\nwhy=y\n"] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }
}
