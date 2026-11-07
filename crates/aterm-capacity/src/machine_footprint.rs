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
//! heavy one), and no less than any run of a selection it holds ([`Selection::holds`]) grew its
//! directories by: runs kept from different builds of the tree disagree (on this Mac on
//! 2026-11-06 a whole-workspace run of every check peaked 23.1 GiB, 21.5 GiB above the floor,
//! below the 29.0 GiB a cold whole-workspace `test --compile-only` grew its directories by), and
//! a wider selection that reads smaller than what a narrower one wrote bounds nothing below that.
//! The claimant's cover is held to the same bound: no less than any run of a selection the
//! claimant holds grew, which a sum of narrower selections' figures may read below.
//! A run claims the least of the covers this record holds for it: its own
//! selection's figure, a wider selection's (one whose work did all of its, [`Selection::work`],
//! over the same crates or more, or the whole workspace: it built every unit the run builds), or
//! the sum of the figures of selections whose work did all of its and whose crates together hold
//! its, each crate's cheapest (each built the units of its crates; the sum counts the
//! dependencies they share once for each). Each is an upper bound on
//! what the run writes, so the least is too, but for one reach: were every run the record keeps
//! begun warm, the floor would stand above what a fresh run begins from, and a run would claim
//! less than it writes by at most the difference. Kept nowhere, no cover: the run claims as
//! before.

use std::io;
use std::path::Path;

use aterm_json::{Map, Value};

use crate::footprint::{Footprint, KEPT};

/// The selections a record keeps runs of: past this, the one whose newest run is oldest goes, so
/// change cones that never come back do not grow it without end.
pub const SELECTIONS: usize = 512;

/// The record's schema: 2 since a word of a selection's work that narrows it holds `--`
/// ([`Selection::work`]). Schema 1 spelled one without (`pulled-cells`), which 2 would read as
/// work done, so a record of 1 reads as none.
pub const SCHEMA: u64 = 2;

/// The most bytes [`MachineFootprints::load`] reads, and [`MachineFootprints::save`] writes.
const FILE_MAX: u64 = 16 << 20;

/// WHAT A RUN BUILT, as the machine's record keeps it: the work, all of which a run bounding it
/// must have done, and the crates it did it for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The work, spelled the same each time it is the same, in words parted by spaces (verify's:
    /// its checks that build, each that builds the whole tree whatever is selected marked `:tree`,
    /// and what narrows them, sorted). A word `X--n` NARROWS the work word `X`, which then does
    /// less than without it (`measure--no-app`, a `measure` that builds no measuring benches; a
    /// list after `=` narrows to its items, `cells--cell=linux,win`), and one that starts `--`
    /// narrows all of the work (`--compile-only`); any other word is work DONE, `X:tree` being
    /// `X` over the whole tree. What makes a check do more is work done, never a narrowing
    /// (`as-release`). A selection's runs bound a run of another work only where they did all of
    /// it ([`Selection::does`]): each word that work does is theirs, their `X:tree` standing for
    /// its `X` (the whole tree holds any selection of it), and each of their words that narrows a
    /// word it does, or all of it, narrows it as far or further, a list to items of theirs. Runs
    /// of other work bound nothing for it.
    pub work: String,
    /// The crates the work was done for, sorted and each once; `None` for the whole workspace,
    /// which holds every selection, and for work that is the same whatever is selected.
    pub crates: Option<Vec<String>>,
}

impl Selection {
    /// Whether this selection holds `other`'s: its work did all of `other`'s ([`Self::does`]),
    /// and the workspace holds every selection's crates, a list one whose every crate it names.
    #[must_use]
    pub fn holds(&self, other: &Selection) -> bool {
        self.does(other) && self.holds_crates(other)
    }

    /// Whether it names every crate `other` does: the workspace names every crate, and a list
    /// none beyond its own.
    fn holds_crates(&self, other: &Selection) -> bool {
        match (&self.crates, &other.crates) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(mine), Some(theirs)) => theirs.iter().all(|c| mine.contains(c)),
        }
    }

    /// Whether this selection's work did all of `other`'s, whatever their crates
    /// ([`Self::work`]'s words): each word `other` does is this one's, or this one did it over
    /// the whole tree (`X:tree` for `X`), and each word that narrows this one's work narrows
    /// `other`'s as far or further where `other` does what it narrows.
    #[must_use]
    pub fn does(&self, other: &Selection) -> bool {
        let done: Vec<&str> = self.work.split_whitespace().collect();
        let asked: Vec<&str> = other.work.split_whitespace().collect();
        let base = |w: &str| w.strip_suffix(":tree").unwrap_or(w).to_owned();
        let work = |words: &[&str]| -> Vec<String> {
            words
                .iter()
                .filter(|w| !w.contains("--"))
                .map(|w| (*w).to_owned())
                .collect()
        };
        let narrowing = |words: &[&str]| -> Vec<(String, String)> {
            words
                .iter()
                .filter_map(|w| w.split_once("--"))
                .map(|(of, how)| (of.to_owned(), how.to_owned()))
                .collect()
        };
        let (done_work, asked_work) = (work(&done), work(&asked));
        let asked_narrowing = narrowing(&asked);
        asked_work
            .iter()
            .all(|w| done_work.iter().any(|d| d == w || base(d) == *w))
            && narrowing(&done).iter().all(|(of, how)| {
                let applies = of.is_empty() || asked_work.iter().any(|a| base(a) == *of);
                !applies
                    || asked_narrowing
                        .iter()
                        .any(|(a_of, a_how)| a_of == of && within(a_how, how))
            })
    }

    /// Whether the work was done for `name`: a list naming it, or the whole workspace.
    #[must_use]
    pub fn names(&self, name: &str) -> bool {
        self.crates
            .as_ref()
            .is_none_or(|c| c.iter().any(|n| n == name))
    }
}

/// Whether the narrowing `asked` (what follows a word's `--`) narrows as far as `done` or further
/// ([`Selection::work`]): the same, or the same list with each of its items `done`'s.
fn within(asked: &str, done: &str) -> bool {
    match (asked.split_once('='), done.split_once('=')) {
        (None, None) => asked == done,
        (Some((name, items)), Some((done_name, done_items))) => {
            name == done_name
                && items
                    .split(',')
                    .all(|i| done_items.split(',').any(|d| d == i))
        }
        _ => false,
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
    /// Whether the growth bound raised it: a run of a selection that the run, or a selection it
    /// was read from, holds grew its directories by more than the figure those selections' own
    /// runs read.
    pub bounded: bool,
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
        self.push_at(at, run);
    }

    fn push_borrowed(&mut self, selection: &Selection, run: Footprint) {
        let at = match self.rows.iter().position(|r| r.selection == *selection) {
            Some(at) => at,
            None => {
                self.rows.push(Kept {
                    selection: selection.clone(),
                    runs: Vec::new(),
                });
                self.rows.len() - 1
            }
        };
        self.push_at(at, run);
    }

    fn push_at(&mut self, at: usize, run: Footprint) {
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
    /// ([`Kept::figure`], above the record's floor, [`Cover::floor`], and no less than any run of
    /// a selection that one holds grew its directories by), a wider selection's
    /// ([`Selection::holds`]), or the sum of the figures of selections whose work did all of its
    /// ([`Selection::does`]) and whose crates together name every crate it names, each crate's
    /// the cheapest naming it; whichever it is, no less than any run of a selection `selection`
    /// holds grew its directories by. `None` where no selection kept covers it: no run that did
    /// all its work, or a crate no such run named.
    #[must_use]
    pub fn cover(&self, selection: &Selection, holds: u64, cold: u64) -> Option<Cover> {
        let floor = self
            .rows
            .iter()
            .flat_map(|r| &r.runs)
            .map(|r| r.peak.saturating_sub(r.grew))
            .min()
            .map_or(holds, |least| least.min(holds));
        // The most any run of a selection `of` holds grew its directories by: `of`'s work did
        // all of that one's over its crates or more, so a run of it writes no less.
        let grown = |of: &Selection| {
            self.rows
                .iter()
                .filter(|q| of.holds(&q.selection))
                .flat_map(|q| &q.runs)
                .map(|q| q.grew)
                .max()
                .unwrap_or(0)
        };
        // Each selection that did all of the run's work: its figure, no less than what a run of a
        // selection it holds grew by, and whether that raised it.
        let same: Vec<(&Kept, u64, bool)> = self
            .rows
            .iter()
            .filter(|r| r.selection.does(selection))
            .filter_map(|r| {
                let f = r.figure(floor, cold)?;
                let g = grown(&r.selection);
                Some((r, f.max(g), g > f))
            })
            .collect();
        let mut best: Option<Cover> = same
            .iter()
            .filter(|(r, _, _)| r.selection.holds(selection))
            .map(|(r, f, bounded)| Cover {
                bytes: *f,
                selections: 1,
                runs: r.runs.len(),
                exact: r.selection == *selection,
                bounded: *bounded,
                floor,
            })
            .min_by_key(|c| (c.bytes, !c.exact, c.selections));
        if let Some(crates) = selection.crates.as_ref().filter(|c| !c.is_empty()) {
            let mut picked: Vec<usize> = Vec::with_capacity(crates.len());
            let all = crates.iter().all(|name| {
                let cheapest = same
                    .iter()
                    .enumerate()
                    .filter(|(_, (r, _, _))| r.selection.names(name))
                    .min_by_key(|(i, (_, f, _))| (*f, *i))
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
                    bounded: picked.iter().any(|i| same[*i].2),
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
        // The run itself does all of the work of each selection it holds, over their crates or
        // more: a sum of narrower selections' figures may read below what one of those grew.
        let least = grown(selection);
        best.map(|c| {
            if least > c.bytes {
                Cover {
                    bytes: least,
                    bounded: true,
                    ..c
                }
            } else {
                c
            }
        })
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
                    out.push_borrowed(
                        &selection,
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
        crate::book::read_text(path, FILE_MAX)
            .ok()
            .flatten()
            .map(|text| Self::decode(&text))
            .unwrap_or_default()
    }

    /// Write the record to `path` ([`crate::book::write_staged`]), so a reader finds the old
    /// file or the new one whole.
    ///
    /// # Errors
    /// As [`crate::book::write_staged`]: the record kept before stands.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        crate::book::write_staged(path, self.encode().as_bytes(), ".disk-footprint-", FILE_MAX)
    }

    /// Keep `run` under `selection` in the record at `path`: read, pushed ([`Self::push`]) and
    /// written back whole, holding the record's lock from the read to the write
    /// ([`crate::book::update_text`]), so two runs ending at once in two checkouts each keep
    /// theirs.
    ///
    /// # Errors
    /// As [`crate::book::update_text`]: an existing record that could not be read is left as it
    /// is.
    pub fn keep(path: &Path, selection: Selection, run: Footprint) -> io::Result<()> {
        crate::book::update_text(path, FILE_MAX, ".disk-footprint-", |text| {
            let mut record = text.map(Self::decode).unwrap_or_default();
            record.push(selection, run);
            Some(record.encode())
        })
        .map(|_| ())
    }
}

#[cfg(test)]
#[path = "machine_footprint_tests.rs"]
mod tests;
