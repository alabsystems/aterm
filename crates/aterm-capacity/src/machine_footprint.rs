// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A KIND OF RUN WROTE IN EVERY CHECKOUT ON THIS MACHINE (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! DISK-ADMIT, DA-MACHINE-FOOTPRINT). A kind whose directories are each checkout's own (verify's
//! build directory in the shared cache, `<cache><checkout>/.cargo-build`, and `<target>/verify`)
//! keeps what its runs took beside them ([`crate::Footprints`]), so a fresh worktree, whose
//! directories hold no run of anything, claimed the kind's whole cold budget (72 GiB for verify)
//! for a run that wrote 3 GiB, and waited for it on a Mac with 170 GB free. This record keeps, for
//! the whole machine, each run that did all its work (verify: every check passed), by what it
//! built ([`Selection`]), so that a run with no run of its scope measured on its own directories
//! claims what runs in this machine's checkouts bound ([`MachineFootprints::cover`]), where that is
//! less than its kind's cold budget less what its directories hold
//! ([`crate::Estimate::or_machine`]): the record only ever lowers a claim.
//!
//! Each run kept bounds what its selection writes into another checkout's directories. A run that
//! passed built every unit of its selection, so its directories held them all at its peak, and a
//! run of it elsewhere writes at most those its directories lack: at most that peak less what
//! every run of the kind holds before it builds anything (for verify, the tool's own build, which
//! every checkout's run begins from). The record reads that as its floor ([`Cover::floor`]): the
//! least that any run it keeps began from (`peak - grew`), or that the claimant's directories
//! hold now where they hold less. A run's figure is its peak beyond the floor, at most the cold
//! budget, or what it grew its directories by where that is more, as
//! [`crate::Footprints::need`] reads a run measured on the directories themselves: a fresh
//! worktree's directories, holding what a fresh run's held, claim what that run grew them by;
//! emptier ones what it takes to bring them to the peak; and fuller ones, a long-lived checkout
//! whose units may serve other selections, no less than a fresh one, however warm the runs kept of
//! the selection began. A selection's figure is the most of its runs' (a light run never hides a
//! heavy one). A run claims the least of the covers this record holds for it: its own
//! selection's figure, a wider selection's (the same work over more crates, or the whole
//! workspace, built every unit it builds), or the sum of the figures of selections of the same
//! work whose crates together hold its, each crate's cheapest (each built the units of its
//! crates; the sum counts the dependencies they share once for each). Each is an upper bound on
//! what the run writes, so the least is too, but for one reach: were every run the record keeps
//! begun warm, the floor would stand above what a fresh run begins from, and a run would claim
//! less than it writes by at most the difference. Kept nowhere, no cover: the run claims as
//! before.

use std::io::{self, Write as _};
use std::path::Path;

use aterm_json::{Map, Value};

use crate::footprint::{Footprint, KEPT};

/// The selections a record keeps runs of: past this, the one whose newest run is oldest goes, so
/// change cones that never come back do not grow it without end.
pub const SELECTIONS: usize = 512;

/// The record's schema.
pub const SCHEMA: u64 = 1;

/// The most bytes [`MachineFootprints::load`] reads, and [`MachineFootprints::save`] writes.
const FILE_MAX: u64 = 16 << 20;

/// WHAT A RUN BUILT, as the machine's record keeps it: the work, which a run bounding it must
/// have done the same, and the crates it did it for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The work, spelled the same each time it is the same: verify's checks that build, sorted,
    /// each that builds the whole tree whatever is selected marked, then whatever else changes
    /// what they write (the cells asked for, a compile-only run). Runs of other work bound
    /// nothing for it.
    pub work: String,
    /// The crates the work was done for, sorted and each once; `None` for the whole workspace,
    /// which holds every selection, and for work that is the same whatever is selected.
    pub crates: Option<Vec<String>>,
}

impl Selection {
    /// Whether this selection holds `other`'s, for the same work: the workspace holds every
    /// selection, and a list one whose every crate it names.
    #[must_use]
    pub fn holds(&self, other: &Selection) -> bool {
        self.work == other.work
            && match (&self.crates, &other.crates) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(mine), Some(theirs)) => theirs.iter().all(|c| mine.contains(c)),
            }
    }

    /// Whether the work was done for `name`: a list naming it, or the whole workspace.
    #[must_use]
    pub fn names(&self, name: &str) -> bool {
        self.crates
            .as_ref()
            .is_none_or(|c| c.iter().any(|n| n == name))
    }
}

/// The runs of one selection, newest last, at most [`KEPT`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    /// What they built.
    pub selection: Selection,
    /// The runs, each tagged with the directories it measured ([`Footprint::dirs`]): its
    /// checkout's.
    pub runs: Vec<Footprint>,
}

impl Kept {
    /// What its runs bound a run of this selection to write, read above `floor` ([`Cover::floor`]):
    /// for each, what it grew its directories by, or what they held at its peak beyond `floor`
    /// but no more than `cold`, and the most of those, as [`crate::Footprints::need`] reads a run
    /// measured on the directories themselves.
    fn figure(&self, floor: u64, cold: u64) -> Option<u64> {
        self.runs
            .iter()
            .map(|r| r.grew.max(r.peak.saturating_sub(floor).min(cold)))
            .max()
    }

    /// When its newest run ended, unix ms.
    fn newest(&self) -> u64 {
        self.runs.iter().map(|r| r.at_ms).max().unwrap_or(0)
    }
}

/// WHAT THIS MACHINE'S RUNS BOUND A RUN TO WRITE ([`MachineFootprints::cover`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cover {
    /// Bytes.
    pub bytes: u64,
    /// The selections it was read from: one (its own, or one holding it), or more, whose crates
    /// together hold its, their figures summed.
    pub selections: usize,
    /// The runs those selections keep.
    pub runs: usize,
    /// Whether it was read from the run's own selection.
    pub exact: bool,
    /// What it read each run's peak beyond: the least that any run the record keeps began from
    /// (what every run of the kind holds before it builds anything, for verify the tool's own
    /// build), or what the run's directories hold now where that is less. Not what they hold now
    /// where that is more: what a long-lived checkout holds beyond what a fresh one does may serve
    /// other selections than the run's.
    pub floor: u64,
}

/// THE RUNS OF ONE KIND IN EVERY CHECKOUT ON THIS MACHINE, by what each built ([`Kept`]): at most
/// [`KEPT`] of each selection, of the [`SELECTIONS`] run most recently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineFootprints {
    rows: Vec<Kept>,
}

impl MachineFootprints {
    /// The selections kept, each with its runs.
    #[must_use]
    pub fn rows(&self) -> &[Kept] {
        &self.rows
    }

    /// Keep `run` under `selection`: its selection's oldest run goes past [`KEPT`], and past
    /// [`SELECTIONS`] the selection whose newest run is oldest.
    pub fn push(&mut self, selection: Selection, run: Footprint) {
        let at = match self.rows.iter().position(|r| r.selection == selection) {
            Some(at) => at,
            None => {
                self.rows.push(Kept {
                    selection,
                    runs: Vec::new(),
                });
                self.rows.len() - 1
            }
        };
        let runs = &mut self.rows[at].runs;
        runs.push(run);
        let over = runs.len().saturating_sub(KEPT);
        runs.drain(..over);
        while self.rows.len() > SELECTIONS {
            let oldest = (0..self.rows.len())
                .filter(|i| *i != at)
                .min_by_key(|i| self.rows[*i].newest());
            match oldest {
                Some(i) => {
                    self.rows.remove(i);
                }
                None => break,
            }
        }
    }

    /// The least this record bounds a run of `selection` to write into directories holding
    /// `holds` bytes, of a kind that writes `cold` into empty ones: its own selection's figure
    /// ([`Kept::figure`], above the record's floor, [`Cover::floor`]), a wider selection's
    /// ([`Selection::holds`]), or the sum of the figures of selections of the same work whose
    /// crates together name every crate it names, each crate's the cheapest naming it. `None`
    /// where no selection kept covers it: no run of the same work, or a crate no run of it named.
    #[must_use]
    pub fn cover(&self, selection: &Selection, holds: u64, cold: u64) -> Option<Cover> {
        let floor = self
            .rows
            .iter()
            .flat_map(|r| &r.runs)
            .map(|r| r.peak.saturating_sub(r.grew))
            .min()
            .map_or(holds, |least| least.min(holds));
        let same: Vec<(&Kept, u64)> = self
            .rows
            .iter()
            .filter(|r| r.selection.work == selection.work)
            .filter_map(|r| r.figure(floor, cold).map(|f| (r, f)))
            .collect();
        let mut best: Option<Cover> = same
            .iter()
            .filter(|(r, _)| r.selection.holds(selection))
            .map(|(r, f)| Cover {
                bytes: *f,
                selections: 1,
                runs: r.runs.len(),
                exact: r.selection == *selection,
                floor,
            })
            .min_by_key(|c| (c.bytes, !c.exact, c.selections));
        if let Some(crates) = selection.crates.as_ref().filter(|c| !c.is_empty()) {
            let mut picked: Vec<usize> = Vec::with_capacity(crates.len());
            let all = crates.iter().all(|name| {
                let cheapest = same
                    .iter()
                    .enumerate()
                    .filter(|(_, (r, _))| r.selection.names(name))
                    .min_by_key(|(i, (_, f))| (*f, *i))
                    .map(|(i, _)| i);
                if let Some(i) = cheapest
                    && !picked.contains(&i)
                {
                    picked.push(i);
                }
                cheapest.is_some()
            });
            if all {
                let candidate = Cover {
                    bytes: picked
                        .iter()
                        .fold(0u64, |sum, i| sum.saturating_add(same[*i].1)),
                    selections: picked.len(),
                    runs: picked.iter().map(|i| same[*i].0.runs.len()).sum(),
                    exact: false,
                    floor,
                };
                if best.as_ref().is_none_or(|b| {
                    (candidate.bytes, !candidate.exact, candidate.selections)
                        < (b.bytes, !b.exact, b.selections)
                }) {
                    best = Some(candidate);
                }
            }
        }
        best
    }

    /// The record as the file holds it, schema [`SCHEMA`]: each selection's work, its crates
    /// (left out for `None`) and its runs.
    #[must_use]
    pub fn encode(&self) -> String {
        let rows = self
            .rows
            .iter()
            .map(|r| {
                let mut o = Map::new();
                o.insert("work".to_owned(), Value::from(r.selection.work.as_str()));
                if let Some(crates) = &r.selection.crates {
                    o.insert(
                        "crates".to_owned(),
                        Value::Array(crates.iter().map(|c| Value::from(c.as_str())).collect()),
                    );
                }
                let runs = r
                    .runs
                    .iter()
                    .map(|f| {
                        let mut q = Map::new();
                        q.insert("peak".to_owned(), Value::from(f.peak));
                        q.insert("grew".to_owned(), Value::from(f.grew));
                        q.insert("at_ms".to_owned(), Value::from(f.at_ms));
                        if let Some(dirs) = f.dirs {
                            q.insert("dirs".to_owned(), Value::from(dirs));
                        }
                        Value::Object(q)
                    })
                    .collect();
                o.insert("runs".to_owned(), Value::Array(runs));
                Value::Object(o)
            })
            .collect();
        let mut file = Map::new();
        file.insert("schema".to_owned(), Value::from(SCHEMA));
        file.insert("rows".to_owned(), Value::Array(rows));
        aterm_json::to_string(&Value::Object(file)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The record `text` holds: none for text that is not [`Self::encode`]'s or of another
    /// schema; a selection it cannot read whole, or a run of one, is left out.
    #[must_use]
    pub fn decode(text: &str) -> MachineFootprints {
        let Ok(file) = aterm_json::from_str::<Value>(text) else {
            return MachineFootprints::default();
        };
        if file.get("schema").and_then(Value::as_u64) != Some(SCHEMA) {
            return MachineFootprints::default();
        }
        let mut out = MachineFootprints::default();
        for r in file
            .get("rows")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(work) = r.get("work").and_then(Value::as_str) else {
                continue;
            };
            let crates = match r.get("crates") {
                None | Some(Value::Null) => None,
                Some(list) => {
                    let Some(names) = list.as_array().and_then(|l| {
                        l.iter()
                            .map(|c| c.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                    }) else {
                        continue;
                    };
                    Some(names)
                }
            };
            let selection = Selection {
                work: work.to_owned(),
                crates,
            };
            for f in r
                .get("runs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let num = |k: &str| f.get(k).and_then(Value::as_u64);
                if let (Some(peak), Some(grew), Some(at_ms)) =
                    (num("peak"), num("grew"), num("at_ms"))
                {
                    out.push(
                        selection.clone(),
                        Footprint {
                            peak,
                            grew,
                            at_ms,
                            dirs: num("dirs"),
                            scope: None,
                        },
                    );
                }
            }
        }
        out
    }

    /// The record kept at `path`; none where it is absent or unreadable, including a file past
    /// 16 MiB: a run then claims as before.
    #[must_use]
    pub fn load(path: &Path) -> MachineFootprints {
        Self::read(path).unwrap_or_default()
    }

    /// An absent record starts empty; any other refusal is not an empty record to a writer.
    fn read(path: &Path) -> io::Result<MachineFootprints> {
        match crate::read_regular_text(path, FILE_MAX) {
            Ok(text) => Ok(Self::decode(&text)),
            Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(why) => Err(why),
        }
    }

    /// Write the record to `path`: staged in a private folder beside it, then renamed over it,
    /// so a reader finds the old file or the new one whole.
    ///
    /// # Errors
    /// The encoded record exceeds the read bound, its directory could not be made, or the write
    /// failed: the record kept before stands.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let encoded = self.encode();
        if u64::try_from(encoded.len()).unwrap_or(u64::MAX) > FILE_MAX {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "the machine's footprint record exceeds its read bound; the one before stands",
            ));
        }
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        crate::ledger::ensure_dir(dir)?;
        let stage = aterm_tempfile::Builder::new()
            .prefix(".disk-footprint-")
            .tempdir_in(dir)?;
        let staged = stage.path().join("data");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options.open(&staged)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if file.metadata()?.permissions().mode() & 0o7777 != 0o600 {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
        }
        file.write_all(encoded.as_bytes())?;
        drop(file);
        std::fs::rename(&staged, path)
    }

    /// Keep `run` under `selection` in the record at `path`: read, pushed ([`Self::push`]) and
    /// written back whole, holding the record's lock ([`crate::book::lock_path`]) from the read
    /// to the write, so two runs ending at once in two checkouts each keep theirs.
    ///
    /// # Errors
    /// The existing record could not be read (it is left as it is), the lock could not be made or
    /// taken, or as [`Self::save`].
    pub fn keep(path: &Path, selection: Selection, run: Footprint) -> io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        crate::ledger::ensure_dir(dir)?;
        let lock = crate::ledger::open_rendezvous(&crate::book::lock_path(path))?;
        let _held = aterm_toolchain::record_lock::RecordLock::lock(lock, 0)?;
        let mut record = Self::read(path)?;
        record.push(selection, run);
        record.save(path)
    }
}

#[cfg(test)]
#[path = "machine_footprint_tests.rs"]
mod tests;
