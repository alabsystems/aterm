// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold's adoption gate (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.2 M6, §11 B11): a Mac
//! writes table operations under fold keys, and a removal mints no Commons epoch, only once every
//! seat heard in the last 7 days reports a build that reads them, or once 60 days have passed
//! since R2 shipped, whichever comes first. Before that the Commons audience keeps its epochs and
//! its heal. Readers open both kinds of operation whatever the gate says: the gate decides only
//! what this Mac writes.
//!
//! The gate is a pure function of what the node service hears of each current seat (its last
//! heard time and the build it reported: a presence row, or before wave B the fleet book's signed
//! entry) and of two compiled facts, [`aterm_types::mesh_switch::FOLD_READS`] and
//! [`aterm_types::mesh_switch::R2_SHIPPED_MS`], which the node service reads once into a [`Gate`]
//! so a test sets either. Once it holds, [`note`] writes a mark in the vault folder, and the mark
//! is sticky: operations sealed under the fold already exist then, so a seat that reappears on an
//! older build is one its updater moves (it shows as needing an update), not a reason to write
//! Commons epochs again. A Mac whose own gate has not held yet while another's has keeps its
//! table writes pending (M4), never lost, until its gate holds.

use crate::Result;
use crate::client::Paths;

/// How recently a seat must have been heard for its build to hold the gate shut (M6).
pub const HEARD_WITHIN_MS: u64 = 7 * DAY_MS;

/// How long after R2 shipped the gate holds whatever the seats run (§11, "Wave D gates, with a
/// bound").
pub const BOUND_MS: u64 = 60 * DAY_MS;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// The mark's file name in the vault's `commons` folder.
const MARK: &str = "fold";

/// What the gate knows of one current seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heard<'a> {
    /// When this Mac last heard the seat (Unix ms), if ever.
    pub last_heard_ms: Option<u64>,
    /// The build it last reported (`0.158.0` or `0.158.0+g<sha>`), if any.
    pub build: Option<&'a str>,
}

/// The gate's compiled facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    /// The first `(major, minor)` whose readers open fold operations; `None` counts no build.
    pub reads: Option<(u64, u64)>,
    /// When R2 shipped (Unix ms); `None` starts no bound.
    pub r2_shipped_ms: Option<u64>,
}

impl Gate {
    /// The facts this build was compiled with.
    #[must_use]
    pub const fn compiled() -> Self {
        Self {
            reads: aterm_types::mesh_switch::FOLD_READS,
            r2_shipped_ms: aterm_types::mesh_switch::R2_SHIPPED_MS,
        }
    }

    /// Whether the fold may turn on at `now_ms` given every current seat's `seats`: the bound has
    /// passed, or every seat heard within [`HEARD_WITHIN_MS`] reported a build that reads fold
    /// keys (a seat with no build is an older aterm's). An empty lab holds nothing open.
    #[must_use]
    pub fn holds(&self, seats: &[Heard<'_>], now_ms: u64) -> bool {
        if self
            .r2_shipped_ms
            .is_some_and(|shipped| now_ms.saturating_sub(shipped) >= BOUND_MS)
        {
            return true;
        }
        let Some(reads) = self.reads else {
            return false;
        };
        let mut heard = seats.iter().filter(|seat| {
            seat.last_heard_ms
                .is_some_and(|at| now_ms.saturating_sub(at) < HEARD_WITHIN_MS)
        });
        let mut any = false;
        heard.all(|seat| {
            any = true;
            seat.build.is_some_and(|build| at_least(build, reads))
        }) && any
    }

    /// The seats among `seats` the gate waits on: heard within [`HEARD_WITHIN_MS`] on a build that
    /// does not read fold keys. What the per-seat view shows as `needs update` once the bound has
    /// passed.
    #[must_use]
    pub fn behind(&self, seats: &[Heard<'_>], now_ms: u64) -> Vec<usize> {
        seats
            .iter()
            .enumerate()
            .filter(|(_, seat)| {
                seat.last_heard_ms
                    .is_some_and(|at| now_ms.saturating_sub(at) < HEARD_WITHIN_MS)
                    && !self
                        .reads
                        .is_some_and(|reads| seat.build.is_some_and(|b| at_least(b, reads)))
            })
            .map(|(at, _)| at)
            .collect()
    }
}

/// Whether `version` (`MAJOR.MINOR.PATCH`, optionally `+<build metadata>`) is `at` or later; a
/// version this cannot read is not.
fn at_least(version: &str, at: (u64, u64)) -> bool {
    let core = match version.split_once('+') {
        Some((_, meta))
            if meta.is_empty()
                || !meta
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')) =>
        {
            return false;
        }
        Some((core, _)) => core,
        None => version,
    };
    let number = |part: &str| {
        (!part.is_empty()
            && (part.len() == 1 || !part.starts_with('0'))
            && part.bytes().all(|b| b.is_ascii_digit()))
        .then(|| part.parse::<u64>().ok())
        .flatten()
    };
    let mut parts = core.split('.');
    match (
        parts.next().and_then(number),
        parts.next().and_then(number),
        parts.next().and_then(number),
        parts.next(),
    ) {
        (Some(major), Some(minor), Some(_), None) => (major, minor) >= at,
        _ => false,
    }
}

/// Whether the fold is on for the vault at `paths`: its mark is there.
#[must_use]
pub fn on(paths: &Paths) -> bool {
    std::fs::symlink_metadata(mark(paths)).is_ok_and(|meta| meta.is_file())
}

/// Where the mark lives: beside the Commons device file, which the roster's state digest reads, so
/// a cached observation is never reused across the mark's arrival.
#[must_use]
pub fn mark(paths: &Paths) -> std::path::PathBuf {
    paths.dir.join("commons").join(MARK)
}

/// Turn the fold on for the vault at `paths` when `gate` holds over `seats` at `now_ms`; whether
/// it is on afterwards. A mark already there stays (the gate is sticky).
///
/// # Errors
/// The mark cannot be written.
pub fn note(paths: &Paths, gate: &Gate, seats: &[Heard<'_>], now_ms: u64) -> Result<bool> {
    if on(paths) {
        return Ok(true);
    }
    if !gate.holds(seats, now_ms) {
        return Ok(false);
    }
    let path = mark(paths);
    if let Some(dir) = path.parent() {
        crate::fsx::ensure_private_dir(dir)?;
    }
    crate::fsx::write_atomic(&path, format!("fold on {now_ms}\n").as_bytes(), 0o600)?;
    Ok(true)
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;
