// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which records version this Mac's writes sign (`docs/DESIGN-vault-log-2026-11-14.md` step 4,
//! tracker FL-148F), and the first format-2 write.
//!
//! - A Mac whose build reads no format 2 ([`crate::crossing::FormatMark::follows`]: while
//!   [`crate::commons::hearing::READS_FORMAT`] names no release, every running build) signs
//!   version 1, judges no fence and keeps no fence file: [`writer`] answers [`Writer::One`]
//!   before it reads anything else.
//! - A Mac whose store reads format 2 (its format mark names it) signs version 2 by its log's
//!   statements ([`crate::membership_log::writer`]).
//! - A Mac with no format mark signs the vault's first format-2 record (FL-D25) only once its
//!   own judgment of [`crate::commons::hearing::FENCE`] held for
//!   [`crate::commons::hearing::FIRST_WRITE`] without a break ([`FenceWatch`], judged at each
//!   records pass and at each write). Any seated member may: one with no admin key as well as
//!   an admin. The write stores the format mark durably before its records file holds the
//!   record ([`crate::crossing::after_format_mark`]). Two Macs that both write first each store
//!   one mark and sign their own records; each follows the other's signed record as any reader
//!   does ([`crate::membership_log::follow`]).
//!
//! A test opens the gate only through its node's faults ([`crate::crossing::Faults::follows`],
//! which also sets the floor to this build's own release, and
//! [`crate::crossing::Faults::now_ms`], the clock).

use super::Paths;
use crate::commons::bootstrap::Bootstrap;
use crate::commons::hearing::{self, FenceWatch, Floor, Ledger};
use crate::crossing::FORMAT_ONE;
use crate::membership_log::{Record, Store, VERSION_TWO};

/// What this Mac's next write signs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Writer {
    /// Record version 1, as every build before format 2.
    One,
    /// Record version 2: the store reads format 2.
    Two,
    /// Record version 2, the vault's first on this Mac: the mark is stored before it is held.
    First,
}

/// The wall clock this Mac judges its fence by and its log pass signs at (Unix milliseconds).
pub(super) fn now_ms(paths: &Paths) -> u64 {
    #[cfg(test)]
    if let Some(now) = crate::crossing::faults(super::format_mark(paths).path()).now_ms {
        return now;
    }
    let _ = paths;
    aterm_time::unix_millis().unwrap_or(0)
}

/// The floor the first write waits on: [`hearing::FENCE`]; in a test whose node reads format 2,
/// this build's own release, which no member below it passes.
fn floor(paths: &Paths) -> Floor {
    #[cfg(test)]
    if crate::crossing::faults(super::format_mark(paths).path()).follows {
        return Floor {
            version: Some(hearing::Version::own()),
            unheard_passes: true,
        };
    }
    let _ = paths;
    hearing::FENCE
}

/// Judge the fence at `now_ms` and keep the watch: `None` where this Mac reads no format 2, its
/// mark names format 2 already, or it holds no Commons roster to judge by.
///
/// # Errors
/// The mark, the roster or the watch's file cannot be read or written.
pub(crate) fn observe(paths: &Paths) -> crate::Result<Option<(FenceWatch, u64)>> {
    let mark = super::format_mark(paths);
    if !mark.follows() || mark.stored()? != FORMAT_ONE {
        return Ok(None);
    }
    let Some(bootstrap) = Bootstrap::cached(paths)? else {
        return Ok(None);
    };
    let Some(access) = bootstrap.access()? else {
        return Ok(None);
    };
    let now = now_ms(paths);
    let ledger = Ledger::read(&bootstrap);
    let holds = ledger
        .holding_below(
            access.members().iter().map(|member| member.binding()),
            now,
            floor(paths),
        )
        .is_empty();
    let before = FenceWatch::read(&bootstrap);
    let after = FenceWatch::observe(before, holds, now);
    if after != before {
        FenceWatch::write(after, &bootstrap)?;
    }
    Ok(after.map(|watch| (watch, now)))
}

/// What this Mac's next write from `store` signs ([`Writer`]): version 1 where the fence cannot
/// be judged ([`observe`] fails), as where it has not held.
///
/// # Errors
/// The format mark cannot be read.
pub(crate) fn writer(paths: &Paths, store: &Store) -> crate::Result<Writer> {
    let mark = super::format_mark(paths);
    if !mark.follows() {
        return Ok(Writer::One);
    }
    if store.reading().admits_two() {
        return Ok(Writer::Two);
    }
    // MUTANT (`crate::crossing::Faults::first_write_unfenced`): no fence.
    #[cfg(test)]
    if crate::crossing::faults(mark.path()).first_write_unfenced && mark.stored()? == FORMAT_ONE {
        return Ok(Writer::First);
    }
    // A fence that cannot be judged now (its roster or its watch unreadable) is no judgment
    // that it held: the write signs version 1, as on a build that judges no fence, and the
    // next judgment that can be made starts the watch again where this one broke it.
    #[cfg(test)]
    let judged = if crate::crossing::faults(mark.path()).fence_error_fails_write {
        observe(paths)?
    } else {
        observe(paths).ok().flatten()
    };
    #[cfg(not(test))]
    let judged = observe(paths).ok().flatten();
    Ok(match judged {
        Some((watch, now)) if watch.held(now) => Writer::First,
        _ => Writer::One,
    })
}

/// Make `store` draft as `writer` signs: for version 2, its log states its registers as this
/// Mac's identity resolves them.
///
/// # Errors
/// This Mac's identity cannot be read.
pub(crate) fn attach(paths: &Paths, writer: Writer, store: &mut Store) -> crate::Result<()> {
    if writer == Writer::One {
        return Ok(());
    }
    if let Some(identity) = super::read_identity(paths)? {
        store.write_two(&identity);
    }
    Ok(())
}

/// Whether `minted` holds a version-2 record.
pub(crate) fn signs_two(minted: &[Record]) -> bool {
    minted.iter().any(|record| record.version() == VERSION_TWO)
}

/// Run `commit`, which holds `minted` in this Mac's records, as `writer` must: a first write
/// that signed version 2 stores the format mark first, durably, and `commit` then reads the
/// records at format 2 ([`crate::crossing::after_format_mark`]); any other runs as it is.
///
/// # Errors
/// The mark cannot be stored (then `commit` does not run), or `commit` fails.
pub(crate) fn commit<T>(
    paths: &Paths,
    writer: Writer,
    minted: &[Record],
    commit: impl FnOnce() -> crate::Result<T>,
) -> crate::Result<T> {
    if writer == Writer::First && signs_two(minted) {
        return crate::crossing::after_format_mark(
            &super::format_mark(paths),
            crate::crossing::FORMAT_TWO,
            commit,
        );
    }
    commit()
}

#[cfg(test)]
#[path = "format_two_tests.rs"]
mod tests;
