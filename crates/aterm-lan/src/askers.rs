// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bundles whose processes asked the broker to consider them for adoption
//! ([`aterm_uds::lan::wire::Request::Consider`], `docs/DESIGN-lan-broker-2026-11-05.md` §3): what
//! the kernel says of each admitted asker, never what it says of itself. The serving side records
//! an asker ([`Askers::asked`]); the adoption loop (`crate::adopt`) reads them and authenticates
//! each before anything is cloned.
//!
//! An asker is its uid, the cdhash of the code the kernel loaded for it and the bundle the kernel
//! records its executable in, kept only for an executable that is a bundle's
//! ([`aterm_uds::lan::install::BUNDLE_EXECUTABLE`]) outside the broker's own image folder. The
//! list is bounded: newest first, one entry per bundle, at most [`MOST_PER_UID`] per uid and
//! [`MOST`] in all, so no account can crowd another's out or grow what the broker keeps. Nothing
//! is read to keep an ask: the serving side reads no file of an asker's bundle, which could stall
//! a connection on a volume that does not answer. The adoption loop gives each asker it
//! authenticated its build ([`Askers::stated`]); past a bound, of the entries other than the one
//! just kept, the one of the lowest build goes (one not yet authenticated the lowest), the oldest
//! of those: the broker adopts the newest build that authenticates, so an ask from an older
//! bundle never pushes out a newer one it authenticated, and the ask just made always stays to be
//! authenticated. The build orders only which entry goes, and decides nothing: authentication
//! does (`crate::adopt`). A process whose image still reads adopting asks again
//! (`aterm_uds::lan::install::consider_for`), so an ask that went is made anew.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The most askers kept.
pub(crate) const MOST: usize = 8;

/// The most askers of one uid kept.
pub(crate) const MOST_PER_UID: usize = 2;

/// One process that asked, as the kernel named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asker {
    /// Its uid.
    pub(crate) uid: u32,
    /// The cdhash of the code the kernel loaded for it.
    pub(crate) cdhash: [u8; 20],
    /// The bundle its executable runs from.
    pub(crate) bundle: PathBuf,
}

/// The askers this broker keeps, newest first, each with the build the adoption loop
/// authenticated for it ([`Askers::stated`]), `None` before it did.
#[derive(Debug, Default)]
pub(crate) struct Askers(Mutex<Vec<(Asker, Option<u64>)>>);

/// The build the bundle at `bundle` states, read with no link on the way to its `Info.plist` and
/// never waiting on a FIFO ([`aterm_uds::lan::install::read_unlinked`]): `None` where it cannot be
/// read or states none. The adoption loop's read of the image it runs.
pub(crate) fn stated_build(bundle: &Path) -> Option<u64> {
    use aterm_uds::lan::install;
    install::read_unlinked(&bundle.join(install::BUNDLE_INFO))
        .ok()
        .and_then(|plist| install::build_number(&plist))
}

/// The bundle `exe` is the executable of: `Some` only for an absolute
/// `<bundle>.app/Contents/MacOS/aterm` outside [`aterm_uds::lan::IMAGE_DIR`] (the broker's own
/// image, which it never adopts again).
pub(crate) fn bundle_of(exe: &Path) -> Option<PathBuf> {
    use aterm_uds::lan::install;
    let bundle = install::source_bundle(exe)?;
    (exe.is_absolute()
        && exe == bundle.join(install::BUNDLE_EXECUTABLE)
        && !exe.starts_with(aterm_uds::lan::IMAGE_DIR))
    .then_some(bundle)
}

impl Askers {
    /// Keep the asker `uid`, whose loaded code is `cdhash` and whose executable the kernel records
    /// at `exe`, first, reading nothing: whether it was kept (an executable that is no bundle's is
    /// not, [`bundle_of`]). An earlier entry for its bundle goes; then, while its uid holds more
    /// than [`MOST_PER_UID`], and then while more than [`MOST`] are kept, the entry other than this
    /// one of the lowest build authenticated for it (none the lowest), the oldest of those.
    pub(crate) fn asked(&self, uid: u32, cdhash: [u8; 20], exe: &Path) -> bool {
        let Some(bundle) = bundle_of(exe) else {
            return false;
        };
        let mut askers = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        askers.retain(|(asker, _)| asker.bundle != bundle);
        askers.insert(
            0,
            (
                Asker {
                    uid,
                    cdhash,
                    bundle,
                },
                None,
            ),
        );
        // The entry to drop where those `of` names (every one, for `None`) are past `most`: of
        // them, other than the one just kept first, the lowest build's, the oldest of those.
        let past = |askers: &[(Asker, Option<u64>)], of: Option<u32>, most: usize| {
            let named = |asker: &Asker| of.is_none_or(|uid| asker.uid == uid);
            if askers.iter().filter(|(asker, _)| named(asker)).count() <= most {
                return None;
            }
            askers
                .iter()
                .enumerate()
                .skip(1)
                .filter(|(_, (asker, _))| named(asker))
                .min_by_key(|(at, (_, build))| (*build, std::cmp::Reverse(*at)))
                .map(|(at, _)| at)
        };
        while let Some(at) = past(&askers, Some(uid), MOST_PER_UID) {
            askers.remove(at);
        }
        while let Some(at) = past(&askers, None, MOST) {
            askers.remove(at);
        }
        true
    }

    /// Give `asker`, where it is still the entry for its bundle, the build the adoption loop
    /// authenticated for it, which orders which entry goes first past a bound ([`Self::asked`]).
    pub(crate) fn stated(&self, asker: &Asker, build: u64) {
        let mut askers = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, stated)) = askers.iter_mut().find(|(kept, _)| kept == asker) {
            *stated = Some(build);
        }
    }

    /// The askers now, newest first.
    pub(crate) fn now(&self) -> Vec<Asker> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(asker, _)| asker.clone())
            .collect()
    }

    /// Forget `asker`, where it is still the entry for its bundle: one whose authentication
    /// failed can never pass it (its cdhash is fixed, the broker's build only rises), and a
    /// process of that bundle that asks again is a new entry.
    pub(crate) fn forget(&self, asker: &Asker) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(kept, _)| kept != asker);
    }
}

#[cfg(test)]
#[path = "askers_tests.rs"]
mod tests;
