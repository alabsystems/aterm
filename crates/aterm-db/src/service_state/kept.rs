// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The accounts service's standing, kept between samples. A derivation is the read the sample
//! has always made ([`accounts_until`]); its answer is given again, stamped with the sample's own
//! times, while every file it was read from shows what it showed. What is kept is the answer the
//! read gave (an availability, a detail, the counts of the account table's cells and how many
//! operations it retains or withholds), the paths it stands under and their stamps: never a
//! record, a value or anything decrypted.

use super::{CURRENT_FOR, Part, ServiceAvailability, ServiceState, Settled, accounts_until};
use aterm_secrets::{
    client::{self, DeskInputs, Observed, Paths, RecordBounds},
    fsx::{self, Look, SeenSet},
};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

/// The table storage of a home that an accounts read lists or reads, each a tree of its own
/// ([`fsx::tree_paths`]): the tables (the catalog and the account proposals among them) and
/// their claims (which the writer succession's seal also hashes), in the primary region and the
/// recovery one, and the cutoffs a removal saved for its repair.
const TABLE_TREES: [&str; 5] = [
    "commons/tables",
    "commons/recovery/tables",
    "commons/table-claims",
    "commons/recovery/table-claims",
    "commons/table-cutoffs",
];

/// How many links a configuration path is followed through before its stamps stop: the read it
/// stands for fails past that many ([`crate::catalog::policy::Policy::read`]), which nothing keeps.
const MAX_LINKS: usize = 40;

/// A kept accounts standing: the state a derivation gave, the paths its key was taken of and the
/// key.
pub(crate) struct KeptAccounts {
    key: AccountsKey,
    listed: Listed,
    /// The state the derivation gave, given again with each sample's times ([`Self::stamped`]).
    template: ServiceState,
}

impl KeptAccounts {
    /// The kept state as a derivation at `now_ms` gives it: observed then, and, where the account
    /// table is readable (Cached), last read successfully then, as every such read sets it.
    fn stamped(&self, now_ms: u64) -> ServiceState {
        let mut state = self.template.clone();
        state.observed_at_ms = now_ms;
        if state.availability == ServiceAvailability::Cached {
            state.last_success_ms = Some(now_ms);
        }
        state
    }
}

/// The folders of a home that an accounts read holds to the owner-only rule before it reads the
/// custody in them, each looked at privately ([`Look::Private`]): the vault folder and its
/// `commons` (`Bootstrap::established_with_journal_until` judges both, and its observation of
/// the snapshot `commons` again). An access-list grant on either refuses that read and moves no
/// mode bit and no time a stamp of a file in them keeps; and `commons`, whose entries come and
/// go, has no stamp of its own in the desk's inputs. The control journal under it is a tree the
/// desk stamps whole, which a grant moves.
fn folder_looks(home: &Paths) -> [(PathBuf, Look); 2] {
    [
        (home.dir.clone(), Look::Private),
        (home.dir.join("commons"), Look::Private),
    ]
}

/// The paths an accounts key is taken of, listed as a derivation began.
#[derive(PartialEq, Eq)]
struct Listed {
    /// Every home of the writer lineage the read walks ([`crate::generation::chain_homes`]),
    /// the one it opens last.
    homes: Vec<Paths>,
    /// Each home's [`folder_looks`].
    folders: Vec<(PathBuf, Look)>,
    /// Each home's [`TABLE_TREES`], whole.
    tables: Vec<(PathBuf, Look)>,
    /// Each home's writer succession folder (but a successor home inside it) and the link to its
    /// predecessor ([`crate::generation::succession_files`]), whole.
    generation: Vec<(PathBuf, Look)>,
    /// The configuration the sharing policy is read from, and each one a selection on the way
    /// names, each with every path its links lead through ([`config_looks`]), whole.
    config: Vec<(PathBuf, Look)>,
}

impl Listed {
    /// What an accounts read of the home at `paths`, its policy read from `config`, reads, as it
    /// stands now; `None` where a tree cannot be listed or its root is a symlink.
    fn of(paths: &Paths, config: &Path) -> Option<Self> {
        let chain = crate::generation::chain_homes(paths);
        let mut folders = Vec::new();
        let mut tables = Vec::new();
        let mut generation = Vec::new();
        for home in &chain.homes {
            folders.extend(folder_looks(home));
            for tree in TABLE_TREES {
                tables.extend(whole(fsx::tree_paths(&home.dir.join(tree), &[])?));
            }
            let (root, successor, link) = crate::generation::succession_files(home);
            generation.extend(whole(fsx::tree_paths(&root, &[successor])?));
            generation.push((link, Look::Whole));
        }
        let mut configs = config_looks(config);
        for named in &chain.configs {
            configs.extend(config_looks(named));
        }
        Some(Self {
            homes: chain.homes,
            folders,
            tables,
            generation,
            config: configs,
        })
    }

    /// The key of these paths now; `None` where a stamp cannot be taken.
    fn key(&self) -> Option<AccountsKey> {
        Some(AccountsKey {
            bounds: client::record_bounds(),
            desk: self.desk()?,
            folders: fsx::seen_set(&self.folders)?,
            tables: fsx::seen_set(&self.tables)?,
            generation: fsx::seen_set(&self.generation)?,
            config: fsx::seen_set(&self.config)?,
        })
    }

    /// Each home's [`client::desk_inputs`] now.
    fn desk(&self) -> Option<Vec<DeskInputs>> {
        self.homes.iter().map(client::desk_inputs).collect()
    }

    /// Every path these stamps stand for: each home's desk files, its folders, the trees, the
    /// succession files and the configuration's.
    #[cfg(test)]
    fn stamped_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .homes
            .iter()
            .flat_map(|home| client::desk_stamped_files(home).expect("the desk's files"))
            .collect();
        paths.extend(
            self.folders
                .iter()
                .chain(&self.tables)
                .chain(&self.generation)
                .chain(&self.config)
                .map(|(path, _)| path.clone()),
        );
        paths
    }
}

/// Each path as [`Look::Whole`].
fn whole(paths: Vec<PathBuf>) -> impl Iterator<Item = (PathBuf, Look)> {
    paths.into_iter().map(|path| (path, Look::Whole))
}

/// `config`, and while it is a symlink each path it links to, each whole: the file the policy is
/// read from, which that read finds by following each link ([`fsx::read_aterm_config`]). A link
/// replaced, or a file at the end rewritten, moves one of these looks; so does a directory link
/// on the way that leads elsewhere, as each look resolves the path's directories afresh.
fn config_looks(config: &Path) -> Vec<(PathBuf, Look)> {
    let mut looks = vec![(config.to_path_buf(), Look::Whole)];
    let mut at = config.to_path_buf();
    for _ in 0..MAX_LINKS {
        let Ok(target) = std::fs::read_link(&at) else {
            break;
        };
        at = at
            .parent()
            .map_or_else(|| target.clone(), |folder| folder.join(&target));
        looks.push((at.clone(), Look::Whole));
    }
    looks
}

/// The stamps an accounts standing is kept under ([`KeptAccounts`]).
#[derive(PartialEq, Eq)]
struct AccountsKey {
    /// The record bounds every membership log and store of this thread takes.
    bounds: RecordBounds,
    /// Each home's vault observation, Commons custody and control journal, by
    /// [`client::desk_inputs`].
    desk: Vec<DeskInputs>,
    /// Each home's [`folder_looks`].
    folders: SeenSet,
    tables: SeenSet,
    generation: SeenSet,
    config: SeenSet,
}

impl AccountsKey {
    /// The first part of this key that `listed` no longer shows, in the order compared: the
    /// record bounds, the desk inputs and the folders (custody), the tables, the succession, the
    /// configuration; `None` where every part stands. A part whose stamps cannot be taken counts
    /// as moved.
    fn moved(&self, listed: &Listed) -> Option<Part> {
        if client::record_bounds() != self.bounds {
            return Some(Part::Vault);
        }
        if listed.desk().as_ref() != Some(&self.desk)
            || fsx::seen_set(&listed.folders).as_ref() != Some(&self.folders)
        {
            return Some(Part::Custody);
        }
        for (part, looks, kept) in [
            (Part::Tables, &listed.tables, &self.tables),
            (Part::Generation, &listed.generation, &self.generation),
            (Part::Config, &listed.config, &self.config),
        ] {
            if fsx::seen_set(looks).as_ref() != Some(kept) {
                return Some(part);
            }
        }
        None
    }
}

/// One accounts sample ([`sample`]).
pub(crate) struct Sampled {
    /// The state, kept or derived, observed at the sample's time.
    pub(crate) state: ServiceState,
    /// Whether it was derived rather than given again.
    pub(crate) derived: bool,
    /// Where a standing was kept and this sample derived again, the part of its key found moved.
    pub(crate) moved: Option<Part>,
}

/// One accounts sample of the home at `paths`, its policy read from `config`, at `now_ms`: the
/// standing `kept`, where every file it was read from stands, and otherwise a derivation, kept in
/// its place where it settled ([`sample_until`]). A derivation gives an active local batch the
/// short admission time to release custody, and reads on until its sample would no longer be
/// current.
pub(crate) fn sample(
    kept: &mut Option<KeptAccounts>,
    paths: &Paths,
    config: &Path,
    now_ms: u64,
) -> Sampled {
    let read_by = Instant::now() + CURRENT_FOR;
    sample_until(
        kept,
        paths,
        config,
        now_ms,
        &crate::admission::deadline,
        read_by,
    )
}

/// [`sample`] whose derivation waits for holders by `wait` and reads by `read_by`
/// ([`accounts_until`]).
///
/// A derivation lists what it reads (the walk's homes, their folders, trees, succession files and
/// configurations, [`Listed::of`]), takes the key of those paths, holds a shared observation of
/// the opened home's vault state lock across the read, lists the paths again and takes the key of
/// the first listing again. It is kept only where that observation settled (held, or no lock
/// there), the answer is a finding of the files ([`Settled::Yes`]), the second listing is the
/// first, and the key after equals the key before; anything else is derived again at the next
/// sample. The listing is taken again because a key taken of a listing stamps only the paths in
/// it: a succession completed, or an entry made in a tree, after the walk or the tree was listed
/// and before the key was taken, shows in the key before as it does in the key after, yet the
/// read may open the home or the entry, whose own files no stamp of the first listing holds.
pub(crate) fn sample_until(
    kept: &mut Option<KeptAccounts>,
    paths: &Paths,
    config: &Path,
    now_ms: u64,
    wait: &dyn Fn() -> Instant,
    read_by: Instant,
) -> Sampled {
    let moved = match kept {
        Some(held) => match held.key.moved(&held.listed) {
            None => {
                return Sampled {
                    state: held.stamped(now_ms),
                    derived: false,
                    moved: None,
                };
            }
            moved => moved,
        },
        None => None,
    };
    let (state, derived) = derive(paths, config, now_ms, wait, read_by);
    *kept = derived;
    Sampled {
        state,
        derived: true,
        moved,
    }
}

/// One derivation, and what it is kept as where it settled ([`sample_until`]).
fn derive(
    paths: &Paths,
    config: &Path,
    now_ms: u64,
    wait: &dyn Fn() -> Instant,
    read_by: Instant,
) -> (ServiceState, Option<KeptAccounts>) {
    let listed = Listed::of(paths, config);
    #[cfg(test)]
    tests::run_hook(tests::Hook::Listed);
    let before = listed.as_ref().and_then(Listed::key);
    let opened = listed
        .as_ref()
        .and_then(|listed| listed.homes.last())
        .unwrap_or(paths);
    // Held across the read and the key after it: no vault writer of the opened home starts
    // while it is.
    let observation = client::observe_commons_source_read(opened);
    let unwritten = matches!(observation, Ok(Observed::Read(_) | Observed::Absent));
    #[cfg(test)]
    tests::run_hook(tests::Hook::Observed);
    let (state, settled) = accounts_until(paths, config, now_ms, wait, read_by);
    #[cfg(test)]
    tests::run_hook(tests::Hook::Derived);
    let relisted = Listed::of(paths, config);
    let after = listed.as_ref().and_then(Listed::key);
    drop(observation);
    let kept = match (listed, after) {
        (Some(listed), Some(after))
            if unwritten
                && settled == Settled::Yes
                && relisted.as_ref() == Some(&listed)
                && before.as_ref() == Some(&after) =>
        {
            Some(KeptAccounts {
                key: after,
                listed,
                template: state.clone(),
            })
        }
        _ => None,
    };
    (state, kept)
}

#[cfg(test)]
#[path = "kept_tests.rs"]
pub(super) mod tests;
