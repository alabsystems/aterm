// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The candidate gate (`trust_compare::compare`) over two real whole-tree lane runs of one aterm
//! tree on one machine, the seal (base) and trust main (candidate), read from the logs
//! targo-trust wrote (`fixtures/candidate-2026-10-02`; its README says what was kept). Real logs
//! give the log reader shapes synthetic cases cannot: a Summary cut short by SIGTERM, a
//! verification that did not run, a complete Coverage line.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::trust_compare::{self, Comparison, Outcome, Ratchet, Retractions};
use crate::trust_facts::Plan;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/it/fixtures/candidate-2026-10-02")
}

/// A copy of both runs a test may edit.
struct Runs {
    dir: aterm_tempfile::TempDir,
}

impl Runs {
    fn new() -> Runs {
        let dir = aterm_tempfile::Builder::new()
            .prefix("candidate-replay")
            .tempdir()
            .unwrap();
        for side in ["base", "candidate"] {
            fs::create_dir_all(dir.path().join(side)).unwrap();
            for e in fs::read_dir(fixture().join(side)).unwrap() {
                let e = e.unwrap();
                fs::copy(e.path(), dir.path().join(side).join(e.file_name())).unwrap();
            }
        }
        Runs { dir }
    }

    fn side(&self, side: &str) -> PathBuf {
        self.dir.path().join(side)
    }

    /// Rewrite `side`'s plan, line by line.
    fn edit_plan(&self, side: &str, edit: impl Fn(&str) -> Option<String>) {
        let p = self.side(side).join(".plan");
        let text: String = fs::read_to_string(&p)
            .unwrap()
            .lines()
            .filter_map(&edit)
            .map(|l| format!("{l}\n"))
            .collect();
        fs::write(p, text).unwrap();
    }

    /// `obligations 1` in both plans: the lane was asked for per-obligation records.
    fn ask_for_records(&self) {
        for side in ["base", "candidate"] {
            self.edit_plan(side, |l| {
                Some(if l.starts_with("crate-budget ") {
                    format!("{l}\nobligations 1")
                } else {
                    l.to_string()
                })
            });
        }
    }

    fn load(&self, side: &str) -> Plan {
        let dir = self.side(side);
        let mut p = Plan::parse(&fs::read_to_string(dir.join(".plan")).unwrap()).unwrap();
        p.logdir = dir;
        p.measure();
        p
    }

    fn compare(&self) -> Comparison {
        trust_compare::compare(
            &self.load("base"),
            &self.load("candidate"),
            None,
            &Ratchet::default(),
            &Retractions::default(),
        )
    }
}

fn outcome(c: &Comparison, name: &str) -> Outcome {
    c.rows
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no row for {name}"))
        .outcome
}

fn with(c: &Comparison, o: Outcome) -> BTreeSet<&str> {
    c.rows
        .iter()
        .filter(|r| r.outcome == o)
        .map(|r| r.name.as_str())
        .collect()
}

/// Every library is decided from its real log (losses with real counts, a SIGTERMed check and an
/// unrun verification as no result, the seal's unfinished check as base undecided, a memory
/// refusal under both as not on this host), and nothing is compared without records.
#[test]
fn the_real_runs_are_decided_from_their_logs_and_not_compared_without_records() {
    let runs = Runs::new();
    let c = runs.compare();
    let set = |names: &[&'static str]| names.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(
        with(&c, Outcome::Loss),
        set(&["aterm-grid", "aterm-scrollback", "smol_str"])
    );
    let deltas: Vec<(String, Option<i128>)> = c
        .rows
        .iter()
        .filter(|r| r.outcome == Outcome::Loss)
        .map(|r| (r.name.clone(), r.delta()))
        .collect();
    assert!(
        deltas.contains(&("aterm-grid".into(), Some(-60))),
        "{deltas:?}"
    );
    assert!(
        deltas.contains(&("aterm-scrollback".into(), Some(-36))),
        "{deltas:?}"
    );
    assert!(
        deltas.contains(&("smol_str".into(), Some(-18))),
        "{deltas:?}"
    );
    assert_eq!(
        with(&c, Outcome::NoResult),
        set(&["aterm-core", "aterm-render"])
    );
    assert_eq!(outcome(&c, "aterm-census"), Outcome::BaseUndecided);
    assert_eq!(outcome(&c, "aterm-cli"), Outcome::NotOnHost);
    assert_eq!(with(&c, Outcome::Ok), set(&["aterm-bits", "astream-cap"]));
    assert_eq!(with(&c, Outcome::Skipped), set(&["aterm-release", "winit"]));
    assert!(
        c.incomparable
            .iter()
            .any(|w| w.contains("captured no per-obligation records")),
        "{:?}",
        c.incomparable
    );
    assert_eq!(trust_compare::render(&c).exit, 2);
}

/// Asked for records and left none, no library passes: a count shortfall is a loss of at least
/// that many obligations, and the rest give no result.
#[test]
fn without_records_no_library_with_a_result_passes() {
    let runs = Runs::new();
    runs.ask_for_records();
    let c = runs.compare();
    assert!(c.incomparable.is_empty(), "{:?}", c.incomparable);
    assert!(with(&c, Outcome::Ok).is_empty());
    assert_eq!(outcome(&c, "aterm-grid"), Outcome::Loss);
    assert_eq!(outcome(&c, "aterm-bits"), Outcome::NoResult);
    assert_eq!(trust_compare::render(&c).exit, 1);
}

/// Runs whose plans name no measured tree (a lane that filed nothing) are not compared.
#[test]
fn runs_that_filed_nothing_are_not_compared() {
    let runs = Runs::new();
    for side in ["base", "candidate"] {
        runs.edit_plan(side, |l| {
            Some(match l {
                l if l.starts_with("file") => "file the checkout was not clean".into(),
                l if l.starts_with("commit ") => "commit -".into(),
                l if l.starts_with("tree ") => "tree -".into(),
                l => l.to_string(),
            })
        });
    }
    let c = runs.compare();
    assert!(!c.incomparable.is_empty());
    assert_eq!(trust_compare::render(&c).exit, 2);
}

/// One made-up record per library with a result, the obligations its Summary counts.
fn record_every_result(runs: &Runs, side: &str) {
    let dir = runs.side(side);
    for e in fs::read_dir(&dir).unwrap() {
        let path = e.unwrap().path();
        if path.extension().is_none_or(|x| x != "log") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        let summary = text
            .lines()
            .rfind(|l| l.contains("Summary: "))
            .unwrap_or("");
        let (Some(p), Some(t)) = crate::trust_facts::parse_summary(summary) else {
            continue;
        };
        let name = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .split('@')
            .next()
            .unwrap()
            .to_string();
        let mut rec = format!(
            "{}\npackage\t{name}\nunit\tcargo-target(package=\"{name}\")\nfunctions\t1\ntotal\t{t}\nproved\t{p}\n",
            crate::trust_oblig::RECORD_MAGIC
        );
        for i in 1..=t {
            let status = if i <= p { "proved" } else { "failed" };
            rec.push_str(&format!(
                "o\t{name}::f\tassertion\tsrc/lib.rs\t{i}\t0\t{i}\t1\t{status}\trow {i}\tassertion\n"
            ));
        }
        rec.push_str(&format!("end\t{t}\n"));
        fs::write(
            format!("{}{}", path.display(), crate::trust_oblig::RECORD_SUFFIX),
            rec,
        )
        .unwrap();
    }
}

/// The seal against itself (one trustc, two directories): refused for what this host could not
/// verify, never for a loss; where the seal has results, a pass with every delta 0.
#[test]
fn the_seal_against_itself_passes_where_it_has_results() {
    let runs = Runs::new();
    let cand = runs.side("candidate");
    fs::remove_dir_all(&cand).unwrap();
    fs::create_dir_all(&cand).unwrap();
    for e in fs::read_dir(runs.side("base")).unwrap() {
        let e = e.unwrap();
        fs::copy(e.path(), cand.join(e.file_name())).unwrap();
    }
    runs.edit_plan("candidate", |l| {
        Some(if l.starts_with("prover ") {
            "prover a519d423f47ff6fb19d505012f210f13ad39f2f6f31cb3c3af5aff508551d0b2".into()
        } else {
            l.to_string()
        })
    });
    runs.ask_for_records();
    record_every_result(&runs, "base");
    record_every_result(&runs, "candidate");
    let c = runs.compare();
    assert!(with(&c, Outcome::Loss).is_empty());
    assert_eq!(
        trust_compare::render(&c).exit,
        1,
        "census, render and cli refuse"
    );
    // Drop the libraries the seal has no result for.
    let unresolved = ["aterm-census", "aterm-render", "aterm-cli"];
    for side in ["base", "candidate"] {
        runs.edit_plan(side, |l| {
            let named = l
                .split_once(' ')
                .and_then(|(k, rest)| {
                    ["crate", "refused", "memory", "argv"]
                        .contains(&k)
                        .then(|| rest.split('\t').next().unwrap_or(""))
                })
                .is_some_and(|n| unresolved.contains(&n));
            (!named).then(|| l.to_string())
        });
    }
    let c = runs.compare();
    assert!(
        c.rows
            .iter()
            .filter(|r| r.outcome == Outcome::Ok)
            .all(|r| r.delta() == Some(0))
    );
    assert_eq!(trust_compare::render(&c).exit, 0);
}

/// Trust main's aterm-grid per obligation (the fixture's `obligations/`): 147 obligations the
/// seal proved and it does not, 87 gained, which offset none.
#[test]
fn trust_mains_grid_loses_147_obligations_and_its_gains_offset_none() {
    let runs = Runs::new();
    for side in ["base", "candidate"] {
        runs.edit_plan(side, |l| {
            let other = l
                .split_once(' ')
                .filter(|(k, _)| ["crate", "refused", "memory", "argv"].contains(k))
                .is_some_and(|(_, rest)| !rest.starts_with("aterm-grid\t"));
            (!other).then(|| l.to_string())
        });
        let record = fixture()
            .join("obligations")
            .join(side)
            .join("aterm-grid@0.101.0.log.oblig.tsv");
        let text = fs::read_to_string(&record).unwrap();
        let field = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{k}\t")))
                .unwrap()
                .to_string()
        };
        fs::copy(
            &record,
            runs.side(side).join("aterm-grid@0.101.0.log.oblig.tsv"),
        )
        .unwrap();
        fs::write(
            runs.side(side).join("aterm-grid@0.101.0.log"),
            format!(
                "Summary: {} proved, 0 failed ({} total)\nCoverage: 1457/1457 eligible function bodies verified (complete)\n",
                field("proved"),
                field("total")
            ),
        )
        .unwrap();
    }
    runs.ask_for_records();
    let c = runs.compare();
    let grid = c.rows.iter().find(|r| r.name == "aterm-grid").unwrap();
    assert_eq!(grid.outcome, Outcome::Loss);
    assert_eq!(grid.lost(), Some(147));
    assert_eq!(grid.delta(), Some(-60));
    match &grid.oblig {
        Some(trust_compare::Oblig::Compared(cmp)) => {
            assert_eq!(cmp.gained, 87);
            assert!(cmp.excused.is_empty());
        }
        other => panic!("{other:?}"),
    }
    let rendered = trust_compare::render(&c);
    assert_eq!(rendered.exit, 1);
    assert_eq!(
        rendered
            .obligations
            .lines()
            .filter(|l| l.starts_with("LOST\taterm-grid\t"))
            .count(),
        147
    );
}
