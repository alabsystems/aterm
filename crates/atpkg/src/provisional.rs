// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! PROVISIONAL builds — the ones laid down by the batteries-included seed that the
//! very next `update` pass may supersede before anyone has used them (§9.1).
//!
//! # Why this exists
//!
//! The sealed registry is a snapshot of the channel at CUT time, and the index
//! publishes on its own cadence — so a machine installing weeks after a cut runs the
//! seed, gets the sealed builds, and then the 6-hour loop's first pass immediately
//! upgrades every program whose pin has moved. Under GC's ordinary retention (live +
//! one rollback) the seed build becomes the retained rollback target, and for `trust`
//! that is ~3.2 GB kept on disk to preserve a state the user occupied for seconds and
//! never ran. On the largest thing aterm installs, that is a doubling nobody asked
//! for and nobody would defend.
//!
//! So the seed records what it installed. While a build is listed here, GC will not
//! keep it as a rollback target ([`crate::gc::reclaimable_with_provisional`]); once it
//! is superseded it is simply reclaimed. Rollback is not lost, only made non-instant:
//! these are published builds, so `atpkg install <program>` fetches one back.
//!
//! # Shape
//!
//! One `program build` pair per line, 0600, best-effort in both directions — an
//! unreadable or absent file means "nothing is provisional", which yields exactly the
//! pre-existing retention behaviour. Nothing here is load-bearing for correctness: the
//! worst outcome of losing this file is that a superseded seed build is retained, i.e.
//! the disk cost this module exists to avoid.
//!
//! Entries are dropped once the build is no longer installed, so the file cannot grow
//! without bound or resurrect meaning for a build number that has been reused.

use std::collections::BTreeSet;

use crate::store::Layout;

/// Record `(program, build)` pairs the seed lane just installed.
///
/// Merges with what is already recorded: a resumed seed pass adds the members it
/// finished without forgetting the ones an earlier pass did.
pub fn record(layout: &Layout, installed: &[(String, u64)]) {
    if installed.is_empty() {
        return;
    }
    let mut all: BTreeSet<(String, u64)> = read(layout).into_iter().collect();
    all.extend(installed.iter().cloned());
    write(layout, &all);
}

/// The provisional build numbers recorded for `program`.
#[must_use]
pub fn builds_for(layout: &Layout, program: &str) -> BTreeSet<u64> {
    read(layout)
        .into_iter()
        .filter(|(p, _)| p == program)
        .map(|(_, b)| b)
        .collect()
}

/// Forget every recorded pair whose build is no longer installed — called after a GC
/// sweep so the record tracks reality rather than accumulating dead entries.
pub fn prune(layout: &Layout) {
    let installed: BTreeSet<(String, u64)> = crate::ops::list_installed(layout).into_iter().collect();
    let kept: BTreeSet<(String, u64)> = read(layout)
        .into_iter()
        .filter(|pair| installed.contains(pair))
        .collect();
    write(layout, &kept);
}

fn read(layout: &Layout) -> Vec<(String, u64)> {
    let Ok(text) = std::fs::read_to_string(layout.provisional()) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let (p, b) = line.trim().split_once(' ')?;
            // A malformed line is skipped, never guessed at: this file only ever
            // costs disk, so silently ignoring junk is the right failure mode.
            Some((p.to_string(), b.trim().parse::<u64>().ok()?))
        })
        .collect()
}

fn write(layout: &Layout, pairs: &BTreeSet<(String, u64)>) {
    let path = layout.provisional();
    if pairs.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let body: String = pairs
        .iter()
        .map(|(p, b)| format!("{p} {b}\n"))
        .collect();
    if let Ok(mut f) = crate::platform::open_create_write(&path, 0o600) {
        use std::io::Write as _;
        let _ = f.write_all(body.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> Layout {
        let dir = std::env::temp_dir().join(format!("atpkg-prov-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Layout { prefix: dir }
    }

    #[test]
    fn records_merges_and_reads_back_per_program() {
        let layout = scratch("rw");
        assert!(builds_for(&layout, "trust").is_empty(), "nothing recorded yet");

        record(&layout, &[("trust".into(), 5520), ("ay".into(), 6255)]);
        assert_eq!(builds_for(&layout, "trust"), [5520].into_iter().collect());
        assert_eq!(builds_for(&layout, "ay"), [6255].into_iter().collect());
        assert!(builds_for(&layout, "clean").is_empty());

        // A resumed pass ADDS without forgetting the earlier one.
        record(&layout, &[("clean".into(), 6319)]);
        assert_eq!(builds_for(&layout, "trust"), [5520].into_iter().collect());
        assert_eq!(builds_for(&layout, "clean"), [6319].into_iter().collect());

        // Junk is skipped, not guessed at — this file only ever costs disk.
        std::fs::write(layout.provisional(), "trust notanumber\nay 6255\n").unwrap();
        assert!(builds_for(&layout, "trust").is_empty());
        assert_eq!(builds_for(&layout, "ay"), [6255].into_iter().collect());

        // An absent file reads as "nothing provisional", which is exactly the
        // pre-existing GC retention behaviour.
        let _ = std::fs::remove_file(layout.provisional());
        assert!(builds_for(&layout, "ay").is_empty());

        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

}
