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
    } else if rest == "config.missing" {
        Entry::new(
            Family::Agh,
            "config",
            "missing",
            "agh.toml missing for the lanes",
            Why::Spent {
                tried: Reason::new("looked for the lanes' agh.toml"),
                reason: Reason::new(
                    "there is none, and they route their GitHub identities through it",
                ),
            },
            Then::step("Provision agh.toml as docs/RUNBOOK-atpkg-republish.md says"),
        )
        .more(&need.what)
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
mod tests {
    use super::*;

    fn need(key: &str) -> AghNeed {
        AghNeed {
            key: key.to_string(),
            what: format!("what {key}"),
            why: format!("why {key}"),
            then: format!("then {key}"),
            at_secs: 7,
        }
    }

    #[cfg(unix)]
    fn json(key: &str, v: u64) -> String {
        format!(
            "{{\"at_secs\":7,\"key\":\"{key}\",\"then\":\"then {key}\",\"v\":{v},\
             \"what\":\"what {key}\",\"why\":\"why {key}\"}}"
        )
    }

    #[cfg(unix)]
    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix("agh-needs-")
            .tempdir()
            .expect("a scratch directory")
    }

    const STALE: &str = "agh.intent.42-0123456789ab.stale";
    const MISSING: &str = "agh.config.missing";
    const INVALID: &str = "agh.config.invalid";

    #[test]
    fn each_need_shape_states_one_entry_under_its_own_key() {
        for key in [STALE, MISSING, INVALID] {
            let made = entries(&[need(key)]);
            assert_eq!(made.len(), 1, "{key}");
            assert_eq!(made[0].key(), key);
            assert_eq!(made[0].family(), Family::Agh);
            assert!(made[0].message().is_ok(), "{key}: {:?}", made[0].message());
        }
    }

    /// `agh.config.invalid` has two causes with two steps (agh's fault, `agh policy check`; a
    /// file that says `vault = true`, which that check accepts, removing the line), so its
    /// entry carries the need's own `then` rather than one fixed step.
    #[test]
    fn an_invalid_config_entry_carries_the_need_s_own_step() {
        let mut vault = need(INVALID);
        vault.then = "remove the vault line".to_string();
        for need in [need(INVALID), vault] {
            let shown = format!("{:?}", entries(std::slice::from_ref(&need))[0].message());
            assert!(shown.contains(&need.then), "{shown}");
        }
    }

    #[test]
    fn a_need_of_another_shape_states_nothing() {
        for key in [
            "agh.other.thing",
            "agh.intent.42-0123456789AB.stale",
            "agh.intent.x-0123456789ab.stale",
            "agh.intent.42-0123456789a.stale",
        ] {
            assert!(entries(&[need(key)]).is_empty(), "{key}");
        }
    }

    /// Each control breaks one rule alone, so dropping that rule from `read` lets a need
    /// through: a symlink to a good need, a good need padded past [`NEED_FILE_MAX`], a good
    /// need whose name does not end `.json` (its stem is its key), a FIFO, which must not
    /// block, then a `v: 2` need and one whose stem is not its key.
    #[cfg(unix)]
    #[test]
    fn read_skips_what_is_not_a_need() {
        use std::os::unix::ffi::OsStrExt as _;
        let root = scratch();
        let dir = root.path().join("needs");
        let outside = root.path().join("outside");
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(dir.join(format!("{MISSING}.json")), json(MISSING, 1)).unwrap();
        std::fs::write(outside.join(format!("{INVALID}.json")), json(INVALID, 1)).unwrap();
        std::os::unix::fs::symlink(
            outside.join(format!("{INVALID}.json")),
            dir.join(format!("{INVALID}.json")),
        )
        .unwrap();
        let big = format!(
            "{{\"at_secs\":7,\"key\":\"agh.big\",\"pad\":\"{}\",\"then\":\"t\",\"v\":1,\
             \"what\":\"w\",\"why\":\"y\"}}",
            "p".repeat(9 << 10)
        );
        std::fs::write(dir.join("agh.big.json"), big).unwrap();
        std::fs::write(dir.join("agh.text.txt"), json("agh.text.txt", 1)).unwrap();
        let fifo = dir.join(format!("{STALE}.json"));
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("a FIFO path");
        // SAFETY: `fifo_c` is a live NUL-terminated pathname in this test's own scratch
        // directory, and mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        std::fs::write(dir.join("agh.two.json"), json("agh.two", 2)).unwrap();
        std::fs::write(dir.join("agh.stem.json"), json("agh.other", 1)).unwrap();
        // A hang detector, not a budget: a read that opened the FIFO would block forever.
        let (tx, rx) = std::sync::mpsc::channel();
        let looked = dir.clone();
        std::thread::spawn(move || tx.send(read(&looked)));
        let needs = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("read returned: a FIFO never blocks it");
        assert_eq!(needs, vec![need(MISSING)]);
        assert_eq!(read(&dir.join("absent")), Vec::new());
    }

    /// A text past [`TEXT_MAX`] is cut, ending `…`, and its need is still read and stated.
    #[cfg(unix)]
    #[test]
    fn a_long_text_is_cut_and_its_need_still_stated() {
        let root = scratch();
        let dir = root.path();
        let long = "y".repeat(1000);
        let text = format!(
            "{{\"at_secs\":7,\"key\":\"{INVALID}\",\"then\":\"t\",\"v\":1,\"what\":\"w\",\
             \"why\":\"{long}\"}}"
        );
        std::fs::write(dir.join(format!("{INVALID}.json")), text).unwrap();
        let needs = read(dir);
        assert_eq!(needs.len(), 1, "{needs:?}");
        assert_eq!(needs[0].why.chars().count(), TEXT_MAX);
        assert!(needs[0].why.ends_with('…'), "{}", needs[0].why);
        let made = entries(&needs);
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].key(), INVALID);
        assert!(made[0].message().is_ok(), "{:?}", made[0].message());
    }

    #[cfg(unix)]
    #[test]
    fn needs_are_stated_at_most_once_a_minute_and_end_when_their_files_go() {
        let root = scratch();
        let dir = root.path().to_path_buf();
        let mut app = crate::App::headless_for_test();
        app.agh_needs = AghNeedsLook::at(dir.clone());
        let under = |app: &crate::App| app.messages.digest_keys_under("agh.");
        let t0 = Instant::now();

        std::fs::write(dir.join(format!("{MISSING}.json")), json(MISSING, 1)).unwrap();
        app.hand_agh_needs(t0);
        assert_eq!(under(&app), vec![MISSING.to_string()]);

        std::fs::write(dir.join(format!("{INVALID}.json")), json(INVALID, 1)).unwrap();
        app.hand_agh_needs(t0 + Duration::from_secs(30));
        assert_eq!(under(&app).len(), 1, "a look is due once a minute");
        app.hand_agh_needs(t0 + Duration::from_secs(61));
        assert_eq!(under(&app).len(), 2);

        std::fs::remove_file(dir.join(format!("{MISSING}.json"))).unwrap();
        std::fs::remove_file(dir.join(format!("{INVALID}.json"))).unwrap();
        app.hand_agh_needs(t0 + Duration::from_secs(122));
        assert_eq!(
            under(&app).len(),
            2,
            "a level entry ends on the digest's clock"
        );
        let due = app.messages.digest_deadline().expect("an end is due");
        let _ = app.settle_messages(due + Duration::from_secs(1));
        assert_eq!(under(&app), Vec::<String>::new());
    }

    /// NEGATIVE CONTROL: Non-need `.json` files (`00.json`..`39.json`) and invalid `agh.*.json`
    /// files sorting lexicographically before real needs (`agh.config.invalid.json`,
    /// `agh.config.missing.json`) must not consume [`NEEDS_MAX`] slots and hide real needs.
    #[cfg(unix)]
    #[test]
    fn non_need_and_corrupt_json_files_sorting_first_do_not_starve_real_needs() {
        let root = scratch();
        let dir = root.path();
        for i in 0..40_u32 {
            std::fs::write(dir.join(format!("{i:02}.json")), "{}").unwrap();
            std::fs::write(dir.join(format!("agh.{i:02}.json")), b"{corrupt").unwrap();
        }
        std::fs::write(dir.join(format!("{INVALID}.json")), json(INVALID, 1)).unwrap();
        std::fs::write(dir.join(format!("{MISSING}.json")), json(MISSING, 1)).unwrap();

        let found = read(dir);
        assert_eq!(found, vec![need(INVALID), need(MISSING)]);
    }
}
