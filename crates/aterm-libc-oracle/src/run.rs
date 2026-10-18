// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One run: decide this host's native cell, phase by phase.
//!
//! Every phase runs that can still mean something, so one run reports every
//! finding; only the controls wait on a green oracle, since a defect planted in
//! an already-red tree proves nothing. A run the machine stops (a missing
//! prerequisite, a full disk) keeps what it found and names the phases it did
//! not reach.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cargo::{Build, DocTarget, LockedFailure, Outcome, Workspace};
use crate::cells::{self, Cell};
use crate::controls::{self, Reference};
use crate::rustdoc::{Doc, FormatError};
use crate::shape;
use crate::siblings::{self, SiblingFinding};
use crate::symbols;
use crate::trust::{self, Trust};
use crate::union::{self, Union};
use crate::verdict::{CouldNotRun, Finding, Phase, PhaseReport, Report, Status};

/// Where to run.
#[derive(Clone, Debug)]
pub struct Options {
    /// The aterm checkout: it holds `libc-oracle/` and `crates/aterm-libc/`.
    pub root: PathBuf,
    /// The oracle workspace's target directory: this checkout's own. Cargo
    /// keys the shim's build by paths relative to the oracle workspace, so a
    /// directory shared with another checkout can serve that checkout's build.
    pub target_dir: PathBuf,
}

/// Told about a run as it goes.
pub trait Observer {
    /// The toolchain answered and this host's cell is known.
    fn deciding(&mut self, host: &str, cell: &Cell);
    /// A phase finished.
    fn phase(&mut self, report: &PhaseReport);
}

/// Decide the native cell of the checkout `options` names.
pub fn run(options: &Options, observer: &mut dyn Observer) -> Report {
    let mut run = Run {
        report: Report::default(),
        observer,
    };
    if let Err(could_not_run) = run.decide(options) {
        run.skip_rest("not reached: the run stopped");
        run.report.could_not_run = Some(could_not_run);
    }
    run.report
}

/// The fix for locked dependencies this machine lacks. `--locked`, so a stale
/// lock is reported online instead of silently rewritten.
const FETCH_ONLINE: &str =
    "fetch them once online: targo fetch --locked --manifest-path libc-oracle/Cargo.toml";

fn stale_lock(outcome: &Outcome) -> Finding {
    Finding::Input {
        what: "libc-oracle/Cargo.lock is stale: the manifests resolve to something else".into(),
        detail: outcome.detail(),
    }
}

/// A refused `targo metadata --locked --offline`, as a finding or as the reason
/// the run stops.
fn metadata_refusal(outcome: &Outcome) -> Result<Finding, CouldNotRun> {
    match outcome.locked_failure() {
        Some(LockedFailure::StaleLock) => Ok(stale_lock(outcome)),
        Some(LockedFailure::NotDownloaded) => Err(CouldNotRun::new(
            format!(
                "the locked dependencies are not on this machine:\n{}",
                outcome.detail()
            ),
            FETCH_ONLINE,
        )),
        Some(LockedFailure::Refused) | None => Ok(Finding::Input {
            what: "libc-oracle's manifests (`targo metadata --locked --offline` refused them)"
                .into(),
            detail: outcome.detail(),
        }),
    }
}

struct Run<'o> {
    report: Report,
    observer: &'o mut dyn Observer,
}

fn started(error: io::Error) -> CouldNotRun {
    if error.kind() == io::ErrorKind::StorageFull {
        return CouldNotRun::new(error.to_string(), "free disk space and rerun");
    }
    CouldNotRun::new(
        format!("targo could not start: {error}"),
        "`aterm pkg doctor`, then `aterm pkg repair`",
    )
}

fn unreadable(error: FormatError) -> CouldNotRun {
    CouldNotRun::new(
        error.to_string(),
        "the trustdoc JSON format changed: update crates/aterm-libc-oracle/src/rustdoc.rs",
    )
}

impl Run<'_> {
    fn phase(&mut self, phase: Phase, since: Instant, detail: String, findings: Vec<Finding>) {
        let status = if findings.is_empty() {
            Status::Ok
        } else {
            Status::Fail
        };
        self.record(phase, since, status, detail);
        self.report.findings.extend(findings);
    }

    /// Name each phase not yet reported as skipped for `why`, once any ran.
    fn skip_rest(&mut self, why: &str) {
        if self.report.phases.is_empty() {
            return;
        }
        for phase in Phase::ALL {
            if !self.report.phases.iter().any(|p| p.phase == phase) {
                self.record(phase, Instant::now(), Status::Skip, why.into());
            }
        }
    }

    fn record(&mut self, phase: Phase, since: Instant, status: Status, detail: String) {
        let line = PhaseReport {
            phase,
            status,
            secs: since.elapsed().as_secs_f64(),
            detail,
        };
        self.observer.phase(&line);
        self.report.phases.push(line);
    }

    fn decide(&mut self, options: &Options) -> Result<(), CouldNotRun> {
        let root = &options.root;
        let oracle = root.join("libc-oracle");
        let shim_src = root.join("crates/aterm-libc/src");
        if !oracle.join("Cargo.toml").is_file() || !shim_src.join("lib.rs").is_file() {
            return Err(CouldNotRun::new(
                format!(
                    "{} holds no libc-oracle/ and crates/aterm-libc/",
                    root.display()
                ),
                "run from an aterm checkout, or pass --root <checkout>",
            ));
        }
        let trust = trust::discover(root)?;
        self.report.host = Some(trust.host.clone());
        let cell = cells::for_host(&trust.host).ok_or_else(|| {
            CouldNotRun::new(
                format!(
                    "this host ({}) is native to no declaration cell",
                    trust.host
                ),
                format!(
                    "run the oracle on a box native to one of {}",
                    cells::triples().join(", ")
                ),
            )
        })?;
        self.report.cell = Some(cell.key);
        self.observer.deciding(&trust.host, cell);

        let ws = Workspace::new(&trust, &oracle, &options.target_dir);
        let union = self.inputs(&ws, root, cell)?;
        // Text, so it means something even when no build can.
        self.siblings(&shim_src);
        let Some(union) = union else {
            self.skip_rest("the inputs are unusable");
            return Ok(());
        };

        let since = Instant::now();
        let test = ws.build(Build::Test).map_err(started)?;
        if test.success {
            let detail = format!(
                "compile-time assertions hold; {} runtime tests passed",
                test.passed
            );
            self.phase(Phase::Conformance, since, detail, Vec::new());
        } else {
            let detail = format!(
                "{} compile errors, {} runtime tests failed ({test})",
                test.errors.len(),
                test.failed.len()
            );
            let finding = Finding::Conformance {
                detail: test.detail(),
            };
            self.phase(Phase::Conformance, since, detail, vec![finding]);
        }

        let Some(reference) = self.surface(&ws, &union)? else {
            self.record(
                Phase::Controls,
                Instant::now(),
                Status::Skip,
                "the shim did not document".into(),
            );
            return Ok(());
        };
        if !test.success {
            self.record(
                Phase::Controls,
                Instant::now(),
                Status::Skip,
                "the oracle is already red; a planted defect would prove nothing".into(),
            );
            return Ok(());
        }
        self.controls(&trust, root, cell, &reference)
    }

    /// The checked-in inputs: a current lockfile, the locked dependencies on
    /// this machine, and the cell's `union.json` names. `None` when an input is
    /// a finding, after which no build can mean anything.
    fn inputs(
        &mut self,
        ws: &Workspace<'_>,
        root: &Path,
        cell: &Cell,
    ) -> Result<Option<Vec<String>>, CouldNotRun> {
        let since = Instant::now();
        let mut findings = Vec::new();
        let mut stop = None;
        let metadata = ws.metadata_locked().map_err(started)?;
        if !metadata.success {
            match metadata_refusal(&metadata) {
                Ok(finding) => findings.push(finding),
                Err(could_not_run) => stop = Some(could_not_run),
            }
        }
        let union = match union_names(root, cell) {
            Ok(names) => names,
            Err(UnionStop::Finding(finding)) => {
                findings.push(finding);
                Vec::new()
            }
            Err(UnionStop::CouldNotRun(could_not_run)) => {
                stop.get_or_insert(could_not_run);
                Vec::new()
            }
        };
        if !findings.is_empty() {
            let detail = match findings.len() {
                1 => "an input is unusable".into(),
                n => format!("{n} inputs are unusable"),
            };
            self.phase(Phase::Inputs, since, detail, findings);
            return stop.map_or(Ok(None), Err);
        }
        if let Some(could_not_run) = stop {
            return Err(could_not_run);
        }
        // Metadata accepted the manifests, so a fetch that refuses is the
        // machine's cache, unless cargo says the lock is stale.
        let fetch = ws.fetch_offline().map_err(started)?;
        if fetch.locked_failure() == Some(LockedFailure::StaleLock) {
            let detail = "Cargo.lock is stale".into();
            self.phase(Phase::Inputs, since, detail, vec![stale_lock(&fetch)]);
            return Ok(None);
        }
        if !fetch.success {
            return Err(CouldNotRun::new(
                format!("targo fetch --locked --offline failed:\n{}", fetch.detail()),
                FETCH_ONLINE,
            ));
        }
        let detail = format!(
            "Cargo.lock current, dependencies offline, {} union names",
            union.len()
        );
        self.phase(Phase::Inputs, since, detail, Vec::new());
        Ok(Some(union))
    }

    fn siblings(&mut self, shim_src: &Path) {
        let since = Instant::now();
        let (detail, findings) = match siblings::read_sources(shim_src) {
            Ok(sources) => {
                let found = siblings::compare(&sources);
                let pairs: Vec<&str> = cells::sibling_pairs()
                    .iter()
                    .map(|(os, _, _)| os.label())
                    .collect();
                let failing: BTreeSet<_> = found.iter().filter_map(SiblingFinding::os).collect();
                let detail = if found.is_empty() {
                    format!("{} body sections identical", pairs.join(" and "))
                } else {
                    format!("{} of {} same-OS pairs fail", failing.len(), pairs.len())
                };
                (detail, found.into_iter().map(Finding::Sibling).collect())
            }
            Err(error) => (
                "the cell sources are unreadable".into(),
                vec![Finding::Input {
                    what: "the shim's cell sources".into(),
                    detail: error.to_string(),
                }],
            ),
        };
        self.phase(Phase::Siblings, since, detail, findings);
    }

    /// Document both crates and compare their shapes and symbols. Returns the
    /// reference over the cell, or `None` when the shim did not document.
    fn surface(
        &mut self,
        ws: &Workspace<'_>,
        union: &[String],
    ) -> Result<Option<Reference>, CouldNotRun> {
        let since = Instant::now();
        let (outcome, path) = ws.rustdoc(DocTarget::Reference).map_err(started)?;
        if !outcome.success {
            return Err(CouldNotRun::new(
                format!(
                    "the reference libc could not be documented under Trust (a toolchain \
                     failure: the shim was not looked at):\n{}",
                    outcome.detail()
                ),
                "`aterm pkg doctor`; a trustdoc that changed its JSON output needs this gate updated",
            ));
        }
        let reference = read_doc(&path)?;
        let reference_public = reference.public().map_err(unreadable)?;
        let expected =
            shape::expected_shape(&reference, &reference_public, union).map_err(unreadable)?;

        let (outcome, path) = ws.rustdoc(DocTarget::Shim).map_err(started)?;
        if !outcome.success {
            let finding = Finding::ShimDoc {
                detail: outcome.detail(),
            };
            self.phase(Phase::Shape, since, format!("{outcome}"), vec![finding]);
            self.record(
                Phase::Symbols,
                Instant::now(),
                Status::Skip,
                "the shim did not document".into(),
            );
            return Ok(None);
        }
        let shim = read_doc(&path)?;
        let shim_public = shim.public().map_err(unreadable)?;
        let actual = shape::public_shape(&shim, &shim_public, None).map_err(unreadable)?;
        let diffs = shape::diff(&expected, &actual);
        let items = shape::item_count(&actual);
        let detail = if diffs.is_empty() {
            format!("{items} public items, each as the reference declares it")
        } else {
            format!("{} shape mismatches over {items} public items", diffs.len())
        };
        self.phase(
            Phase::Shape,
            since,
            detail,
            diffs.into_iter().map(Finding::Shape).collect(),
        );

        let since = Instant::now();
        let reference_symbols = symbols::extern_symbols(&reference_public).map_err(unreadable)?;
        let shim_symbols = symbols::extern_symbols(&shim_public).map_err(unreadable)?;
        let diffs = symbols::diff(&reference_symbols, &shim_symbols);
        let mut detail = format!(
            "{} extern fns, {} redirected",
            shim_symbols.len(),
            symbols::redirected(&shim_symbols)
        );
        if !diffs.is_empty() {
            detail.push_str(&format!("; {} bound unlike the reference", diffs.len()));
        }
        let findings = diffs.into_iter().map(Finding::Symbol).collect();
        self.phase(Phase::Symbols, since, detail, findings);
        Ok(Some(Reference {
            shape: expected,
            symbols: reference_symbols,
        }))
    }

    fn controls(
        &mut self,
        trust: &Trust,
        root: &Path,
        cell: &'static Cell,
        reference: &Reference,
    ) -> Result<(), CouldNotRun> {
        let since = Instant::now();
        let report = controls::run(trust, root, cell, reference)?;
        let mut detail = format!("{} planted defects caught", report.caught.len());
        if !report.failed.is_empty() {
            detail.push_str(&format!(", {} not", report.failed.len()));
        }
        if !report.not_on_this_cell.is_empty() {
            let names: Vec<String> = report.not_on_this_cell.iter().map(|c| c.name()).collect();
            detail.push_str(&format!(
                "; {} decided on the other family's cells",
                names.join(", ")
            ));
        }
        let findings = report
            .failed
            .into_iter()
            .map(|(control, failure)| Finding::Control { control, failure })
            .collect();
        self.phase(Phase::Controls, since, detail, findings);
        Ok(())
    }
}

/// Why the cell's names could not be had from `union.json` ([`union_names`]).
#[derive(Debug)]
enum UnionStop {
    /// The file is missing: the checkout lacks an input, so nothing is decided.
    CouldNotRun(CouldNotRun),
    /// The file is unreadable or malformed, or names no list for the cell.
    Finding(Finding),
}

/// The cell's names from the checkout's `union.json`. A missing file decides nothing
/// (COULD NOT RUN, exit 3): no build can say anything about a cell whose list is
/// absent. A file that is there and unusable stays a finding (FAIL, exit 1).
fn union_names(root: &Path, cell: &Cell) -> Result<Vec<String>, UnionStop> {
    let finding = |detail| {
        UnionStop::Finding(Finding::Input {
            what: union::PATH.into(),
            detail,
        })
    };
    match Union::read(root) {
        Ok(union) => union.names(cell).map_err(finding),
        Err(union::Unread::Absent(detail)) => Err(UnionStop::CouldNotRun(CouldNotRun::new(
            format!("the checkout holds no {} ({detail})", union::PATH),
            format!("restore it: git checkout -- {}", union::PATH),
        ))),
        Err(union::Unread::Unusable(detail)) => Err(finding(detail)),
    }
}

fn read_doc(path: &Path) -> Result<Doc, CouldNotRun> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        CouldNotRun::new(
            format!(
                "rustdoc reported success but {} is unreadable: {e}",
                path.display()
            ),
            "rerun; if it persists, `aterm pkg doctor`",
        )
    })?;
    Doc::parse(&text).map_err(unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing `union.json` decides nothing (COULD NOT RUN, exit 3); one that is
    /// there and malformed is a finding (FAIL, exit 1).
    #[test]
    fn a_missing_union_could_not_run_and_a_malformed_one_is_a_finding() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("libc-oracle")).expect("mkdir");
        let cell = &cells::CELLS[0];
        match union_names(dir.path(), cell) {
            Err(UnionStop::CouldNotRun(c)) => {
                assert!(c.what.contains(union::PATH), "{c}");
                assert!(c.fix.is_some_and(|f| f.contains("git checkout")));
            }
            other => panic!("a missing union.json must not run: {other:?}"),
        }
        std::fs::write(union::path(dir.path()), "{").expect("write");
        assert!(
            matches!(
                union_names(dir.path(), cell),
                Err(UnionStop::Finding(Finding::Input { .. }))
            ),
            "a malformed union.json is a finding"
        );
        std::fs::write(union::path(dir.path()), "{}").expect("write");
        assert!(
            matches!(
                union_names(dir.path(), cell),
                Err(UnionStop::Finding(Finding::Input { .. }))
            ),
            "a union.json without the cell's list is a finding"
        );
    }
}
