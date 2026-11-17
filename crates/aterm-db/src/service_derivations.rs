// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the services' observer counts of its own samples ([`Derivations`]), by the part of a
//! kept standing's key it found moved ([`Part`]): plain counts, built on every platform. The
//! observer that makes them is [`crate::service_state`]'s, which builds on Unix; a reader of
//! them (aterm-gui's network inspection) builds everywhere, and on a platform with no observer
//! it reads none.

/// A part of a kept standing's key: what [`Derivations::moved`] counts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The secrets service's selection inputs, or a plain vault's source
    /// ([`aterm_secrets::client::service_reading::Moved::Vault`]); the record bounds of the accounts service's.
    Vault,
    /// The vault observation, Commons custody and control journal of a home of the accounts
    /// service's writer lineage ([`aterm_secrets::client::desk_inputs`]), and the owner-only verdicts of its
    /// vault folder and `commons`.
    Custody,
    /// The table storage of such a home: the tables, their claims and the saved cutoffs, in
    /// each region.
    Tables,
    /// The writer succession's own files ([`crate::generation::succession_files`]).
    Generation,
    /// The aterm configuration the sharing policy is read from, through each link.
    Config,
}

impl Part {
    /// Every part, in the order of their indexes in [`Derivations::moved`].
    pub const ALL: [Self; 5] = [
        Self::Vault,
        Self::Custody,
        Self::Tables,
        Self::Generation,
        Self::Config,
    ];

    /// Its index in [`Derivations::moved`].
    const fn index(self) -> usize {
        match self {
            Self::Vault => 0,
            Self::Custody => 1,
            Self::Tables => 2,
            Self::Generation => 3,
            Self::Config => 4,
        }
    }
}

/// How many samples a services observer ([`crate::service_state::Observer`]) took, how many of them derived each service's standing rather
/// than giving the one kept, and for those that derived where one was kept, which part of its key
/// was found moved first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Derivations {
    /// The observer's samples.
    pub samples: u64,
    /// The secrets service's derivations.
    pub secrets: u64,
    /// The accounts service's derivations.
    pub accounts: u64,
    /// Derivations of either service whose kept standing's key moved, by the part first found
    /// moved, at [`Part`]'s index: vault, custody, tables, generation, config.
    pub moved: [u64; 5],
}

impl Derivations {
    /// The derivations counted for `part` in [`Self::moved`].
    #[must_use]
    pub const fn moved_by(&self, part: Part) -> u64 {
        self.moved[part.index()]
    }

    /// Count one sample of a service that `derived` ([`Self::secrets`] or [`Self::accounts`],
    /// `count`), and the part of its key that `moved`. Unix only, as its caller (the service
    /// state) is.
    #[cfg(unix)]
    pub(crate) fn count(count: &mut u64, moved: &mut [u64; 5], derived: bool, part: Option<Part>) {
        if derived {
            *count = count.saturating_add(1);
        }
        if let Some(part) = part {
            moved[part.index()] = moved[part.index()].saturating_add(1);
        }
    }
}
