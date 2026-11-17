// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The secrets service's standing for Network State, kept between samples. A derivation reads
//! the vault's files as the services sample always read them ([`observe_materialization`] after
//! [`status`]); its answer is given again, with no file opened, while every file it read shows
//! what it showed then. What is kept is the answer (a membership face, an entry count, a blocker),
//! the stamps it stands under and `state.toml`'s receipts: never a value, a record or anything
//! decrypted.

use super::*;
use crate::fsx::{Look, SeenSet};

/// What Network State shows of this Mac's membership: [`Membership`] without what it never
/// shows, when the vault last heard this Mac and the version a revoked Mac read last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// [`Membership::Unconfigured`].
    Unconfigured,
    /// [`Membership::Pending`].
    Pending,
    /// [`Membership::Revoked`].
    Revoked,
    /// [`Membership::Plain`].
    Plain,
    /// [`Membership::In`], at the sequence it holds.
    In {
        /// The sequence held.
        seq: u64,
    },
}

impl Face {
    /// What Network State shows of `membership`.
    #[must_use]
    pub const fn of(membership: &Membership) -> Self {
        match membership {
            Membership::Unconfigured => Self::Unconfigured,
            Membership::Pending => Self::Pending,
            Membership::Revoked { .. } => Self::Revoked,
            Membership::Plain => Self::Plain,
            Membership::In { seq, .. } => Self::In { seq: *seq },
        }
    }
}

/// The secrets service's standing as a derivation found it, without when it found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// This Mac's local vault metadata ([`status`]) could not be read.
    Unreadable,
    /// No vault is configured: this Mac's membership and how many entries its state names.
    Unconfigured {
        /// What Network State shows of the membership.
        membership: Face,
        /// How many entries the applied state names ([`Status::names`]).
        entries: usize,
    },
    /// The configured vault passed the readiness guard ([`observe_materialization`]): this Mac's
    /// membership and how many entries its state names, read under that guard.
    Ready {
        /// What Network State shows of the membership.
        membership: Face,
        /// How many entries the applied state names ([`Status::names`]).
        entries: usize,
    },
    /// The readiness guard's blocker.
    Blocked(MaterializationBlocker),
    /// The readiness guard's read failed.
    Unverified,
}

impl Standing {
    /// Whether a read of the same files gives this answer again: not one whose read failed, nor
    /// one that met a writer holding the state lock or a configuration that moved while it read.
    const fn of_the_files(&self) -> bool {
        !matches!(
            self,
            Self::Unreadable
                | Self::Unverified
                | Self::Blocked(
                    MaterializationBlocker::WriterBusy | MaterializationBlocker::ConfigChanged
                )
        )
    }
}

/// One reading of the secrets service ([`Reader::read`]).
#[derive(Clone, Debug)]
pub struct Reading {
    /// The standing, kept or derived.
    pub standing: Standing,
    /// The receipts `state.toml` retains for the vault configured ([`observe_receipts`]): those
    /// last read where a writer holds the state lock, for contention is not a failed read, and
    /// none where no vault folder, lock or configuration is there. `None` where they could not be
    /// read: the refresh history is unreadable.
    pub receipts: Option<Receipts>,
    /// Whether this reading derived its standing from the files, rather than giving the one kept.
    pub derived: bool,
    /// Where a standing was kept and this reading derived again, the first part of its key found
    /// moved, in the order they are compared; `None` where nothing was kept or the kept standing
    /// stood.
    pub moved: Option<Moved>,
}

/// A part of a kept standing's key ([`Reader`]), the first found moved ([`Reading::moved`]); a
/// part whose stamps cannot be taken counts as moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    /// The selection inputs ([`vault_memo::selection_inputs`]), or a plain vault's source file.
    Vault,
}

/// The secrets service's reader of one vault folder, which keeps its last settled standing under
/// the stamps of every file its derivation read (its key) and gives it again while they stand.
///
/// The key is the vault memos' selection inputs ([`vault_memo::selection_inputs`]: the vault
/// folder's identity, [`vault_memo::observation_inputs`] by their metadata but `state.toml` by
/// what it holds but `heard` and the receipts, the membership records' standing and the record
/// bounds) and a plain vault's source file ([`plain_looks`]). The vault writes no file under the
/// home, so the readiness guard reads none there but the vault folder's and a plain vault's
/// source, wherever `source.toml` puts it (`~/team.secrets` included). A standing is kept only
/// from a derivation made under a settled observation of the state lock (held, or no lock there),
/// that folded no fault of the moment ([`Judgement::faulted`]), whose answer is one of the files
/// ([`Standing::of_the_files`]), and around which no stamp moved: the selection inputs and the
/// plain source's taken before the derivation, each equal to those taken after it. Anything else
/// is derived again at the next read, as every read derived before.
///
/// The receipts are read apart, under their own key (the vault folder's identity and the whole
/// metadata of `.lock`, `source.toml`, `vault`, the identity and `state.toml`, whose receipts are
/// in the fields its standing leaves out), and given again while it stands.
pub struct Reader {
    paths: Paths,
    kept: Option<Kept>,
    /// The receipts last read, with the key they stand under where it held across their read.
    receipts: Option<(Option<SeenSet>, Receipts)>,
    derived: u64,
}

/// A kept standing, the stamps it stands under and the paths they were taken of.
struct Kept {
    key: SecretsKey,
    /// [`plain_looks`] of the configuration the derivation read, where it names one.
    plain: Option<Vec<(PathBuf, Look)>>,
    standing: Standing,
}

impl Kept {
    /// The first part of the key whose files no longer show what they showed, where one does
    /// not; `None` where every file the kept standing was read from stands. The selection inputs
    /// are compared first, then a plain vault's source.
    fn moved(&self, paths: &Paths) -> Option<Moved> {
        if !vault_memo::selection_inputs(paths).is_some_and(|inputs| inputs == self.key.inputs) {
            return Some(Moved::Vault);
        }
        match &self.plain {
            Some(looks)
                if !fsx::seen_set(looks)
                    .is_some_and(|seen| self.key.plain.as_ref() == Some(&seen)) =>
            {
                Some(Moved::Vault)
            }
            Some(_) | None => None,
        }
    }
}

/// The stamps a standing is kept under ([`Reader`]).
#[derive(PartialEq, Eq)]
struct SecretsKey {
    inputs: vault_memo::SelectionInputs,
    plain: Option<SeenSet>,
}

impl SecretsKey {
    /// The stamps now of the selection inputs and of `plain`; `None` where a metadata read fails
    /// (read again then).
    fn now(paths: &Paths, plain: Option<&[(PathBuf, Look)]>) -> Option<Self> {
        Some(Self {
            inputs: vault_memo::selection_inputs(paths)?,
            plain: match plain {
                Some(looks) => Some(fsx::seen_set(looks)?),
                None => None,
            },
        })
    }
}

impl Reader {
    /// A reader of the vault folder at `paths`, with nothing kept.
    #[must_use]
    pub const fn new(paths: Paths) -> Self {
        Self {
            paths,
            kept: None,
            receipts: None,
            derived: 0,
        }
    }

    /// How many readings derived their standing ([`Reading::derived`]).
    #[must_use]
    pub const fn derivations(&self) -> u64 {
        self.derived
    }

    /// The secrets service's standing and receipts now: those kept, where every file they were
    /// read from shows what it showed, and otherwise read again. Nothing it reads is written,
    /// repaired or waited for, but a state lock an older build left wide, which is narrowed as the
    /// vault memos narrow it before they observe it ([`StateLock::narrow_owned`]).
    pub fn read(&mut self) -> Reading {
        let receipts = self.receipts_now();
        let moved = match &self.kept {
            Some(kept) => match kept.moved(&self.paths) {
                None => {
                    return Reading {
                        standing: kept.standing.clone(),
                        receipts,
                        derived: false,
                        moved: None,
                    };
                }
                moved => moved,
            },
            None => None,
        };
        self.derived = self.derived.saturating_add(1);
        let (standing, kept) = derive(&self.paths);
        self.kept = kept;
        Reading {
            standing,
            receipts,
            derived: true,
            moved,
        }
    }

    /// The receipts: those last read where their key stands; otherwise one read
    /// ([`observe_configured_receipts`](super::receipts::observe_configured_receipts)), kept under
    /// the key taken before it where that held across it. A writer holding the state lock gives
    /// the receipts last read, and a folder with no lock none.
    fn receipts_now(&mut self) -> Option<Receipts> {
        let looks = receipt_looks(&self.paths);
        let before = fsx::seen_set(&looks);
        if let Some((Some(key), receipts)) = &self.receipts
            && before.as_ref() == Some(key)
        {
            return Some(*receipts);
        }
        match super::receipts::observe_configured_receipts(&self.paths) {
            Ok(Observed::Read(read)) => {
                let key = before.filter(|before| fsx::seen_set(&looks).as_ref() == Some(before));
                self.receipts = Some((key, read));
                Some(read)
            }
            Ok(Observed::Busy) => Some(
                self.receipts
                    .as_ref()
                    .map_or_else(Receipts::default, |(_, receipts)| *receipts),
            ),
            Ok(Observed::Absent) => {
                self.receipts = None;
                Some(Receipts::default())
            }
            Err(_) => None,
        }
    }
}

/// What the receipts are read from ([`observe_receipts`]): the vault folder by its identity, and
/// the state lock (its presence and mode decide the observation), `source.toml` and `vault` (the
/// configuration and its chain), the identity and `state.toml`, each whole.
fn receipt_looks(paths: &Paths) -> [(PathBuf, Look); 6] {
    [
        (paths.dir.clone(), Look::Identity),
        (paths.lock_file(), Look::Whole),
        (paths.source_file(), Look::Whole),
        (paths.cache_file(), Look::Whole),
        (paths.identity_file(), Look::Whole),
        (paths.state_file(), Look::Whole),
    ]
}

/// One derivation of the standing, and what it is kept as where nothing moved around it
/// ([`Reader`]).
fn derive(paths: &Paths) -> (Standing, Option<Kept>) {
    // A lock an older build left wide is narrowed before it is observed, as the vault memos
    // narrow it, so the first read after an upgrade can settle; its stamps are taken after.
    let narrowed = StateLock::narrow_owned(paths);
    let before = vault_memo::selection_inputs(paths);
    // Held across the derivation and the stamps after it: no writer can start while it is.
    let observation = narrowed.and_then(|_| StateLock::observe_classified(paths));
    let settled = matches!(
        observation,
        Ok(StateObservation::Held(_) | StateObservation::Absent)
    );
    #[cfg(test)]
    run_hook(At::Observed);
    let mut judgement = Judgement::default();
    let (standing, plain) = derived(paths, &mut judgement);
    #[cfg(test)]
    run_hook(At::Derived);
    let plain_now = plain.as_ref().map(|plain| plain.looks.as_slice());
    let kept = SecretsKey::now(paths, plain_now).and_then(|after| {
        let unmoved = settled
            && !judgement.faulted
            && standing.of_the_files()
            && before.as_ref() == Some(&after.inputs)
            && plain.as_ref().and_then(|plain| plain.before.as_ref()) == after.plain.as_ref();
        unmoved.then(|| Kept {
            key: after,
            plain: plain.map(|plain| plain.looks),
            standing: standing.clone(),
        })
    });
    drop(observation);
    (standing, kept)
}

/// Paths a derivation reads and their stamps, taken before it read them.
struct Looked {
    looks: Vec<(PathBuf, Look)>,
    /// `None` where a metadata read failed.
    before: Option<SeenSet>,
}

impl Looked {
    /// `looks` with their stamps now.
    fn now(looks: Vec<(PathBuf, Look)>) -> Self {
        let before = fsx::seen_set(&looks);
        Self { looks, before }
    }
}

/// The derivation proper, the services sample's read as it has always been: [`status`], and for
/// a configured vault the readiness guard [`observe_materialization`] makes, with [`status`] read
/// again under it, `judgement` learning what it folded. The plain source's looks and stamps are
/// returned, taken before the guard.
fn derived(paths: &Paths, judgement: &mut Judgement) -> (Standing, Option<Looked>) {
    let local = match status(paths) {
        Ok(local) => local,
        Err(_) => return (Standing::Unreadable, None),
    };
    let Some(config) = &local.config else {
        return (
            Standing::Unconfigured {
                membership: Face::of(&local.membership),
                entries: local.names.len(),
            },
            None,
        );
    };
    let plain = plain_looks(config).map(Looked::now);
    let observed = with_ready_state_reusing(
        paths,
        Some(config),
        local_judged,
        None,
        None,
        OnWriter::Answer,
        judgement,
        |_| status(paths),
    );
    let standing = match observed {
        Ok(MaterializationObservation::Ready(held)) => Standing::Ready {
            membership: Face::of(&held.inputs.membership),
            entries: held.inputs.names.len(),
        },
        Ok(MaterializationObservation::Blocked(blocker)) => Standing::Blocked(blocker),
        Err(_) => Standing::Unverified,
    };
    (standing, plain)
}

/// What a plain vault's selection reads beside the vault folder (`plain_entries`, through
/// [`materialization_selection`]'s selection of an unsigned vault at a path): the source file
/// `source.toml` names, whole, and its folder, by its identity. `None` for any other vault, whose
/// selection reads no such file.
fn plain_looks(config: &Config) -> Option<Vec<(PathBuf, Look)>> {
    let Source::Path(path) = &config.source else {
        return None;
    };
    if config.signer.is_some() {
        return None;
    }
    let mut looks = vec![(path.clone(), Look::Whole)];
    if let Some(folder) = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    {
        looks.push((folder.to_path_buf(), Look::Identity));
    }
    Some(looks)
}

/// Where in a derivation a test's hook runs ([`with_hook`]).
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum At {
    /// Once the state lock is observed, before the derivation reads.
    Observed,
    /// Once the derivation has read, before its stamps are taken again.
    Derived,
}

/// A test's hook and the point it runs at ([`with_hook`]).
#[cfg(test)]
type Hook = (At, Box<dyn FnMut()>);

#[cfg(test)]
std::thread_local! {
    /// What a test runs at a point of each derivation on this thread ([`with_hook`]).
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

/// Run the hook a test set for the point `at` of this thread's derivations.
#[cfg(test)]
fn run_hook(at: At) {
    HOOK.with(|hook| {
        if let Some((point, hook)) = hook.borrow_mut().as_mut()
            && *point == at
        {
            hook();
        }
    });
}

/// Run `read` with `hook` run on this thread at the point `at` of each derivation: a writer that
/// changes a file while a derivation reads, or lets go of the state lock once it was observed.
#[cfg(all(test, unix))]
pub(super) fn with_hook<T>(at: At, hook: impl FnMut() + 'static, read: impl FnOnce() -> T) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            HOOK.with(|hook| hook.borrow_mut().take());
        }
    }
    HOOK.with(|slot| *slot.borrow_mut() = Some((at, Box::new(hook))));
    let _clear = Clear;
    read()
}

#[cfg(all(test, unix))]
impl Reader {
    /// Every path the kept standing's key and the receipts' key look at: the selection inputs'
    /// files ([`vault_memo::stamped_files`] of [`vault_memo::observation_inputs`], `state.toml`
    /// among them) and the vault folder, and the plain source's looks. `None` with no standing
    /// kept.
    pub(super) fn stamped_paths(&self) -> Option<Vec<PathBuf>> {
        let kept = self.kept.as_ref()?;
        let mut paths = vault_memo::stamped_files(&self.paths, vault_memo::observation_inputs)?;
        paths.push(self.paths.dir.clone());
        paths.extend(kept.plain.iter().flatten().map(|(path, _)| path.clone()));
        paths.extend(receipt_looks(&self.paths).map(|(path, _)| path));
        Some(paths)
    }
}
