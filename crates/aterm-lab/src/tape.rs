// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAB'S HARNESS TAPES. A lab's instance records every hosted loop it
//! runs (the development seam `ATERM_HARNESS_TAPE`, which [`seam`] names and
//! `lab::start_instance` sets), into its state root's tape store
//! (`<state>/drive/tapes/<sid>/`, `aterm_agent::supervise::tape::store`).
//! `down` keeps the whole `drive/` directory, tapes included; a scenario run
//! whose lab is its own ends each loop as a person does first ([`end_loops`]),
//! so each tape carries its end, then copies them beside its records and says
//! what they cost in `summary.json` ([`summary`]); and the selftest replays
//! each one ([`replay_all`]): a tape the lab's own harness recorded must run
//! again, offline, from itself alone.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::supervise::tape::codec::Item;
use aterm_agent::supervise::tape::{replay, report, show, store};

use crate::door::Door;
use crate::stop::{self, Stopped};

/// The development seam an instance is started with so its hosted loops are
/// taped: the name and its value.
#[must_use]
pub fn seam() -> (String, String) {
    (store::HOSTED_SEAM.to_string(), "1".to_string())
}

/// Every tape file under `dir`, at any depth (a session's directory, `kept/`),
/// in path order.
fn tapes_under(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let kind = std::fs::symlink_metadata(&d)?.file_type();
        if !kind.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a tape directory",
                d.display()
            )));
        }
        for entry in std::fs::read_dir(&d)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(io::Error::other(format!(
                    "{} is a link in the tape inventory",
                    entry.path().display()
                )));
            }
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if kind.is_file() && entry.path().extension().is_some_and(|x| x == "atape") {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Whether the tape at `path` carries its run's end: its loop ended (a tape
/// whose loop the instance's end cut off mid-wait has none, and replays only
/// as ENDED). A tape that does not read has none.
fn ended(path: &Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|raw| show::open(&raw, false).ok())
        .is_some_and(|tape| tape.items.iter().any(|i| matches!(i, Item::End(_))))
}

/// The sessions under the store `dir` with a tape that has no end yet — a
/// loop still recording — each by its directory's name, which is its sid
/// without the `@` (`store::session_dir`); `kept/` is a person's, never one.
fn recording(dir: &Path) -> io::Result<Vec<String>> {
    let mut sids = Vec::new();
    for path in tapes_under(dir)? {
        if ended(&path) {
            continue;
        }
        let sid = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("an open tape has no UTF-8 session directory"))?;
        if sid != store::KEPT {
            sids.push(sid.to_string());
        }
    }
    sids.sort();
    sids.dedup();
    Ok(sids)
}

/// END EVERY LOOP STILL RECORDING under the store `dir` as a person ends it:
/// its session's Autopilot turned off (`ctl @<sid> meta set autopilot off`),
/// which stops the session's supervisor at its next look, so its tape
/// carries its end. `down`'s TERM would end the instance with the loop
/// mid-wait and its tape endless, and a copy taken while it runs is cut off
/// where the copy read it: either replays only as ENDED, and what it counts
/// is a prefix. Only a lab that is the caller's own is ended so: an
/// attached one's Autopilot is its owner's. The wait is a hang detector (a
/// minute), never a budget; whether every inventoried tape now carries its end.
/// An incomplete inventory returns `Ok(false)`, never an empty-success proof.
/// The control calls and filesystem calls have their own inherited bounds.
///
/// # Errors
/// Interrupted.
pub fn end_loops(door: &Door<'_>, dir: &Path) -> Result<bool, Stopped> {
    let open = match recording(dir) {
        Ok(open) => open,
        Err(_) => return Ok(false),
    };
    if open.is_empty() {
        return Ok(true);
    }
    for sid in &open {
        let _ = door.ctl(&[&format!("@{sid}"), "meta", "set", "autopilot", "off"]);
    }
    let stop_by = Instant::now() + Duration::from_secs(60);
    loop {
        match recording(dir) {
            Ok(open) if open.is_empty() => return Ok(true),
            Err(_) => return Ok(false),
            Ok(_) => {}
        }
        if Instant::now() >= stop_by {
            return Ok(false);
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

/// What the tapes under a directory cost and hold ([`summary`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cost {
    /// The tapes.
    pub tapes: u64,
    /// The tapes with no end: cut off mid-run (a lab not this run's to
    /// end, a loop that did not stop), what they count only a prefix.
    pub open: u64,
    /// Their bytes.
    pub bytes: u64,
    /// The door crossings on them: round trips, outputs and flags.
    pub doors: u64,
    /// Their decisions.
    pub decisions: u64,
}

/// What the tapes under `dir` cost and hold, for `summary.json`. A tape that
/// is malformed counts its bytes and stays open; an inventory or read error
/// refuses a partial count. Concurrent mutation/copy completeness is not certified.
pub fn summary(dir: &Path) -> Result<Cost, String> {
    let mut cost = Cost::default();
    for path in tapes_under(dir).map_err(|e| format!("incomplete tape inventory: {e}"))? {
        let raw =
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        cost.tapes += 1;
        cost.bytes += raw.len() as u64;
        // A tape copied while its run still wrote may end torn: what it holds
        // up to the tear is counted (the reader drops the tear and says so),
        // and it is counted open.
        match show::open(&raw, true) {
            Ok(tape) => {
                if !tape.items.iter().any(|i| matches!(i, Item::End(_))) {
                    cost.open += 1;
                }
                let s = show::stats(&tape);
                cost.doors += s.doors.values().map(|d| d.records as u64).sum::<u64>();
                cost.decisions += s.decisions.values().map(|n| *n as u64).sum::<u64>();
            }
            Err(_) => cost.open += 1,
        }
    }
    Ok(cost)
}

/// Each tape under `dir` replayed strictly (`aterm drive replay`'s replay, in
/// this process): its path and the verdict's word, or why it could not be
/// replayed.
#[must_use]
pub fn replay_all(dir: &Path) -> Vec<(PathBuf, Result<report::Verdict, String>)> {
    // A listing failure is one failed observation, not an empty successful
    // result. The selftest's existing per-result failure path must see it.
    let tapes = match tapes_under(dir) {
        Ok(tapes) => tapes,
        Err(e) => {
            return vec![(
                dir.to_path_buf(),
                Err(format!("incomplete tape inventory: {e}")),
            )];
        }
    };
    tapes
        .into_iter()
        .map(|path| {
            let verdict = std::fs::read(&path)
                .map_err(|e| format!("cannot read it: {e}"))
                .and_then(|raw| replay::replay(&raw, &replay::Options::default()))
                .map(|r| r.verdict);
            (path, verdict)
        })
        .collect()
}

#[cfg(test)]
mod inventory_tests {
    use super::*;

    #[test]
    fn an_inventory_error_is_reported_instead_of_looking_empty() {
        let owned = aterm_tempfile::tempdir().expect("owned scratch");
        let root = owned.path().join("tapes");
        std::fs::create_dir(&root).expect("tapes");
        assert!(replay_all(&root).is_empty(), "an ordinary empty inventory");
        let bad_tape = root.join("bad.atape");
        std::fs::write(&bad_tape, b"not a harness tape").expect("owned file");
        let ordinary = replay_all(&root);
        assert_eq!(ordinary.len(), 1, "the ordinary file is actually visited");
        assert_eq!(ordinary[0].0, bad_tape);
        assert!(ordinary[0].1.is_err(), "a malformed tape is reported");
        let wrong_type = owned.path().join("not-a-directory");
        std::fs::write(&wrong_type, b"occupied").expect("owned wrong type");
        let missing = owned.path().join("missing");
        let reported: Vec<bool> = [&wrong_type, &missing]
            .into_iter()
            .map(|dir| {
                let result = replay_all(dir);
                result.len() == 1 && result[0].0 == *dir && result[0].1.is_err()
            })
            .collect();
        assert!(
            reported.iter().all(|yes| *yes),
            "tape inventory errors must be reported, never an empty readable subset: {reported:?}"
        );
        assert_eq!(
            std::fs::read(&wrong_type).expect("still owned"),
            b"occupied"
        );
    }
}

#[cfg(test)]
mod completeness_postfix_tests {
    use super::*;
    #[test]
    fn recording_and_cost_refuse_an_incomplete_inventory() {
        let owned = aterm_tempfile::tempdir().expect("exclusive fixture");
        let root = owned.path().join("tapes");
        std::fs::create_dir(&root).expect("owned directory");
        assert!(
            recording(&root)
                .expect("empty complete inventory")
                .is_empty()
        );
        assert_eq!(
            summary(&root).expect("empty complete count"),
            Cost::default()
        );
        let malformed = root.join("bad.atape");
        std::fs::write(&malformed, b"not a tape").expect("owned malformed tape");
        assert_eq!(summary(&root).expect("malformed is counted open").open, 1);
        let wrong = owned.path().join("wrong-type");
        std::fs::write(&wrong, b"occupied").expect("owned wrong type");
        let missing = owned.path().join("missing");
        for path in [&wrong, &missing] {
            assert!(
                recording(path).is_err() && summary(path).is_err(),
                "incomplete tape inventory is not a completed loop or partial cost"
            );
        }
        assert_eq!(
            std::fs::read(&wrong).expect("occupied retained"),
            b"occupied"
        );
    }
}
