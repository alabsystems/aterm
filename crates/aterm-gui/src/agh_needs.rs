// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The need files agh and atpkg-publish leave for a person (`<state>/agh/needs/<key>.json`,
//! written by `agh::needs`), stated as the level family `Family::Agh`.
//!
//! aterm-gui links no agh, so it reads agh's small format itself. Both sides take the state
//! root from `aterm_types::dirs::resolve_state_dir` over their own process's inputs, so the
//! window misses a lane's needs on Linux when the unit's `XDG_STATE_HOME` is not the window's,
//! and anywhere the window runs under a state override (`ATERM_STATE_HOME`, a test root). A
//! shipped build on macOS reads the same directory the lanes write.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_messages::Severity;
use aterm_messages::exceptions::{Entry, Family};
use aterm_messages::need::{Reason, Then, Why};

/// One need, as its file holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AghNeed {
    /// `[a-z0-9._-]{1,96}`, starting `agh.`: the file's stem.
    pub(crate) key: String,
    /// What needs a look, in one line.
    pub(crate) what: String,
    /// Why agh left it, in its own words.
    pub(crate) why: String,
    /// The one action.
    pub(crate) then: String,
    /// When agh raised it, in Unix seconds.
    pub(crate) at_secs: u64,
}

/// The most needs one look keeps.
pub(crate) const NEEDS_MAX: usize = 32;
/// The most directory entries scanned and candidate `.json` files opened in one look.
const NEED_SCAN_MAX: usize = 1024;
const NEED_OPEN_MAX: usize = 128;
/// The largest file a look reads: agh's `NEED_MAX`, 8 KiB.
pub(crate) const NEED_FILE_MAX: u64 = 8 << 10;
/// How often the window looks.
pub(crate) const LOOK_EVERY: Duration = Duration::from_secs(60);

/// The most characters of a need's `what`, `why` or `then` the window keeps: a longer one is
/// cut ([`clipped`]), never skipped, because agh bounds only the file ([`NEED_FILE_MAX`]).
const TEXT_MAX: usize = 400;

/// `text` cut to [`TEXT_MAX`] characters, the last of them `…` where it was cut. The texts
/// only feed an entry's detail lines, so a long one is shortened, and a need agh wrote is
/// never lost to its length.
fn clipped(text: &str) -> String {
    if text.chars().count() <= TEXT_MAX {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(TEXT_MAX - 1).collect();
    cut.push('…');
    cut
}

/// Whether `key` is a need's key: 1 to 96 bytes of `[a-z0-9._-]`, starting `agh.`.
fn key_ok(key: &str) -> bool {
    key.len() <= 96
        && key.starts_with("agh.")
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// The need a file's bytes hold, when they are one: `v` 1, a key of the pattern equal to
/// `stem`, three string texts (each [`clipped`] to [`TEXT_MAX`] characters), and an unsigned
/// `at_secs`.
fn parse(stem: &str, bytes: &[u8]) -> Option<AghNeed> {
    let doc: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
    if doc.get("v")?.as_u64()? != 1 {
        return None;
    }
    let key = doc.get("key")?.as_str()?;
    if !key_ok(key) || key != stem {
        return None;
    }
    let text = |name: &str| doc.get(name)?.as_str().map(clipped);
    Some(AghNeed {
        key: key.to_string(),
        what: text("what")?,
        why: text("why")?,
        then: text("then")?,
        at_secs: doc.get("at_secs")?.as_u64()?,
    })
}

#[cfg(unix)]
fn owned_by_current_uid(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.uid() == aterm_uds::peer::our_uid())
}

#[cfg(not(unix))]
fn owned_by_current_uid(_path: &Path) -> bool {
    true
}

/// Every need in `dir`, by key.
///
/// A missing directory is an empty list. Candidate `<key>.json` names whose stem passes
/// [`key_ok`] are sorted and bounded ([`NEED_SCAN_MAX`], [`NEED_OPEN_MAX`]); each must be a
/// regular file owned by this user (a symlink is skipped) of at most [`NEED_FILE_MAX`], opened so
/// a FIFO never blocks the window, and hold a valid need ([`parse`]), keeping at most
/// [`NEEDS_MAX`] valid needs so non-need or corrupt files cannot starve real ones.
pub(crate) fn read(dir: &Path) -> Vec<AghNeed> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = listing
        .take(NEED_SCAN_MAX)
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| {
            let Some(stem) = name.strip_suffix(".json") else {
                return false;
            };
            if !key_ok(stem) {
                aterm_log::debug!("agh needs: {name} is not a need key, skipped");
                return false;
            }
            true
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .take(NEED_OPEN_MAX)
        .filter_map(|name| {
            let stem = name.strip_suffix(".json").unwrap_or(&name);
            let path = dir.join(&name);
            let need = owned_by_current_uid(&path)
                .then(|| aterm_uds::runs::read_bounded_regular_text(&path, NEED_FILE_MAX).ok())
                .flatten()
                .and_then(|text| parse(stem, text.as_bytes()));
            if need.is_none() {
                aterm_log::debug!("agh needs: {name} is not a need, skipped");
            }
            need
        })
        .take(NEEDS_MAX)
        .collect()
}

/// Whether `id` is an intent id: `<digits>-<12 lowercase hex>`.
fn intent_id_ok(id: &str) -> bool {
    id.split_once('-').is_some_and(|(step, hex)| {
        !step.is_empty()
            && step.bytes().all(|b| b.is_ascii_digit())
            && aterm_codec::hex::is_lower(hex, 12)
    })
}

/// The entry one need states, or `None` for a key of a shape this build does not know.
fn entry(need: &AghNeed) -> Option<Entry> {
    let rest = need.key.strip_prefix("agh.")?;
    let entry = if let Some(id) = rest
        .strip_prefix("intent.")
        .and_then(|r| r.strip_suffix(".stale"))
        .filter(|id| intent_id_ok(id))
    {
        Entry::new(
            Family::Agh,
            &format!("intent.{id}"),
            "stale",
            "agh write never settled",
            Why::Spent {
                tried: Reason::new("sent an agh write"),
                reason: Reason::new("its answer was lost, and agh never sends a write twice"),
            },
            Then::step("Run agh intent list --open"),
        )
        .more(&need.what)
        .more(&need.why)
        .more(&need.then)
    } else if rest == "config.invalid" {
        Entry::new(
            Family::Agh,
            "config",
            "invalid",
            "agh.toml invalid for the lanes",
            Why::Spent {
                tried: Reason::new("read the lanes' agh.toml"),
                reason: Reason::new(
                    "agh reads no part of an invalid file, and the lanes route through it",
                ),
            },
            // agh's own fault says `agh policy check`; a file that says `vault = true`,
            // which that check accepts, says the step that removes the line.
            Then::step("Clear the fault as the need's last line says"),
        )
        .more(&need.why)
        .more(&need.then)
    } else {
        return None;
    };
    Some(
        entry
            .severity(Severity::Warn)
            .log_line(format!("agh need {}", need.key)),
    )
}

/// The entries the needs state, one per need of a known shape.
pub(crate) fn entries(needs: &[AghNeed]) -> Vec<Entry> {
    needs
        .iter()
        .filter_map(|need| {
            let made = entry(need);
            if made.is_none() {
                aterm_log::debug!("agh needs: {} is no shape this build states", need.key);
            }
            made
        })
        .collect()
}

/// The window's look at the needs.
#[derive(Debug, Default)]
pub(crate) struct AghNeedsLook {
    /// When the next look is due; `None` before the first.
    pub(crate) due: Option<Instant>,
    /// The needs the last statement named; `None` until one is made, or after a restate.
    pub(crate) handed: Option<Vec<AghNeed>>,
    /// The needs directory; `None` is an inert look.
    pub(crate) dir: Option<PathBuf>,
}

impl AghNeedsLook {
    /// A look at `dir`.
    pub(crate) fn at(dir: PathBuf) -> AghNeedsLook {
        AghNeedsLook {
            dir: Some(dir),
            ..AghNeedsLook::default()
        }
    }

    /// `AghNeedsLook::at` of `aterm_types::dirs::state_dir()/agh/needs`, or
    /// `AghNeedsLook::default()`, an inert look, when there is no state directory.
    pub(crate) fn production() -> AghNeedsLook {
        aterm_types::dirs::state_dir().map_or_else(AghNeedsLook::default, |state| {
            AghNeedsLook::at(state.join("agh").join("needs"))
        })
    }

    /// Forget what was handed, so the next look states the whole answer again.
    pub(crate) fn restate(&mut self) {
        self.handed = None;
    }
}

impl crate::App {
    /// State agh's needs as [`Family::Agh`], at most once every [`LOOK_EVERY`]: when they
    /// differ from what was handed, or nothing was handed yet, the whole answer replaces the
    /// family's entries. A need is a fact for later, so this adds no ripening deadline.
    pub(crate) fn hand_agh_needs(&mut self, now: Instant) {
        let Some(dir) = self.agh_needs.dir.clone() else {
            return;
        };
        if self.agh_needs.due.is_some_and(|due| now < due) {
            return;
        }
        self.agh_needs.due = Some(now + LOOK_EVERY);
        let needs = read(&dir);
        if self.agh_needs.handed.as_ref() == Some(&needs) {
            return;
        }
        self.except_replace(Family::Agh, &entries(&needs));
        self.agh_needs.handed = Some(needs);
    }
}

#[cfg(test)]
#[path = "agh_needs_tests.rs"]
mod tests;
