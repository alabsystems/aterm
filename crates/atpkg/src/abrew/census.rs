// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Pre-lock admission census over Homebrew's formula and cask indexes (`lab abrew-admission`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use aterm_json::Value;

use super::api::{self, Catalog};
use super::cask;
use super::formula::{Cask, Formula};
use super::install::{self, InstallError, InstallOpts};
use super::machine::Machine;
use super::prefix::Prefix;

/// What of a population admission admits, and what refuses the rest.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// The admitted packages' names, in index order.
    pub admitted: Vec<String>,
    /// How many packages were refused.
    pub refused: usize,
    /// Map from masked refusal reason to `(packages_refused, refused_solely_by_this)`.
    pub refusals: BTreeMap<String, (usize, usize)>,
}

impl Tally {
    fn count(&mut self, name: &str, refusals: Vec<InstallError>) {
        if refusals.is_empty() {
            self.admitted.push(name.to_string());
            return;
        }
        self.refused += 1;
        let alone = usize::from(refusals.len() == 1);
        let reasons: BTreeSet<String> = refusals
            .into_iter()
            .map(|e| masked(&e.message(), name))
            .collect();
        for why in reasons {
            let (all, sole) = self.refusals.entry(why).or_default();
            *all += 1;
            *sole += alone;
        }
    }

    /// Refusals with (packages refused, alone), most common first.
    #[must_use]
    pub fn ranked(&self) -> Vec<(&str, usize, usize)> {
        let mut ranked: Vec<(&str, usize, usize)> = self
            .refusals
            .iter()
            .map(|(why, (all, sole))| (why.as_str(), *all, *sole))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        ranked
    }
}

/// One census of both indexes.
#[derive(Debug)]
pub struct Census {
    /// [`Machine::describe`] of the machine judged for.
    pub machine: String,
    /// SHA-256 of `formula.json` and `cask.json` as read.
    pub formula_index: String,
    pub cask_index: String,
    /// Formula records in the index.
    pub formulae: usize,
    /// Each formula on its own (`install --ignore-dependencies`).
    pub solo: Tally,
    /// Each formula admitted solo, judged with its runtime closure (`install`).
    pub closure: Tally,
    /// Cask records in the index.
    pub casks: usize,
    /// Each cask with its `depends_on` formulae and casks (`install --cask`).
    pub cask: Tally,
}

/// The downloaded indexes as admission's catalog: records found by name.
struct Index<'v> {
    machine: &'v Machine,
    formulae: HashMap<&'v str, &'v Value>,
    casks: HashMap<&'v str, &'v Value>,
}

impl Catalog for Index<'_> {
    fn machine(&self) -> &Machine {
        self.machine
    }

    fn formula(&self, name: &str) -> Result<Formula, String> {
        let short = Formula::short_name(name);
        let record = self
            .formulae
            .get(short)
            .ok_or_else(|| format!("abrew: no such formula: {short}"))?;
        api::formula_record(record, short, self.machine)
    }

    fn cask(&self, token: &str) -> Result<Cask, String> {
        let raw_short = Formula::short_name(token);
        let short = api::canonical_cask_token(raw_short);
        let record = self
            .casks
            .get(short)
            .ok_or_else(|| format!("abrew: no such cask: {raw_short}"))?;
        api::cask_record(record, token)
    }
}

/// Judge every record of `formula_index` and `cask_index` for `machine`.
pub fn measure(
    formula_index: &[u8],
    cask_index: &[u8],
    machine: &Machine,
) -> Result<Census, String> {
    let parse = |bytes: &[u8], what: &str| -> Result<Vec<Value>, String> {
        match aterm_json::from_slice(bytes) {
            Ok(Value::Array(records)) => Ok(records),
            Ok(_) => Err(format!("abrew: the {what} index is not a list of records")),
            Err(e) => Err(format!("abrew: the {what} index: {e}")),
        }
    };
    let formula_records = parse(formula_index, "formula")?;
    let cask_records = parse(cask_index, "cask")?;
    let key = |record: &'_ Value, field: &str| -> String {
        record
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let formula_names: Vec<String> = formula_records.iter().map(|r| key(r, "name")).collect();
    let cask_tokens: Vec<String> = cask_records.iter().map(|r| key(r, "token")).collect();
    let index = Index {
        machine,
        formulae: formula_names
            .iter()
            .map(String::as_str)
            .zip(&formula_records)
            .collect(),
        casks: cask_tokens
            .iter()
            .map(String::as_str)
            .zip(&cask_records)
            .collect(),
    };
    let scratch = aterm_tempfile::tempdir().map_err(|e| format!("abrew: census scratch: {e}"))?;
    let prefix = Prefix::at(&scratch.path().join("prefix"));
    let mut census = Census {
        machine: machine.describe(),
        formula_index: super::fetch::sha256_bytes(formula_index),
        cask_index: super::fetch::sha256_bytes(cask_index),
        formulae: formula_records.len(),
        solo: Tally::default(),
        closure: Tally::default(),
        casks: cask_records.len(),
        cask: Tally::default(),
    };
    let alone = InstallOpts {
        ignore_dependencies: true,
        quiet: true,
        ..InstallOpts::default()
    };
    let whole = InstallOpts {
        quiet: true,
        ..InstallOpts::default()
    };
    for name in &formula_names {
        let solo = install::preflight_refusals(&index, &prefix, name, &alone);
        let admitted = solo.is_empty();
        census.solo.count(name, solo);
        if admitted {
            census.closure.count(
                name,
                install::preflight_refusals(&index, &prefix, name, &whole),
            );
        }
    }
    for token in &cask_tokens {
        let verdict = index
            .cask(token)
            .map_err(InstallError::Failed)
            .and_then(|c| {
                cask::preflight(&prefix, &c, machine)
                    .and_then(|()| cask::preflight_deps(&index, &prefix, &c))
            });
        census
            .cask
            .count(token, verdict.err().into_iter().collect());
    }
    Ok(census)
}

/// `why` with the refused package's own `name` spelled `<name>`: its first
/// whole-word occurrence, where every refusal names its subject (a later one
/// is prose, as in `cask installer ships a pkg installer`). A word is a run
/// of the bytes a package name is made of; a sentence's full stop after it
/// is not part of it.
fn masked(why: &str, name: &str) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || "._+@-".contains(c);
    let mut out = String::with_capacity(why.len());
    let mut rest = why;
    let mut pending = !name.is_empty();
    while !rest.is_empty() {
        let (run, tail) = rest.split_at(rest.find(|c| !word(c)).unwrap_or(rest.len()));
        let stem = run.trim_end_matches('.');
        if pending && stem == name {
            pending = false;
            out.push_str("<name>");
            out.push_str(&run[stem.len()..]);
        } else {
            out.push_str(run);
        }
        let (gap, next) = tail.split_at(tail.find(word).unwrap_or(tail.len()));
        out.push_str(gap);
        rest = next;
    }
    out
}

fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

impl Census {
    /// Format the admission totals and top ten refusals for each category as text.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = format!(
            "abrew admission for {}: formula.json {}, cask.json {}\n",
            self.machine,
            &self.formula_index[..self.formula_index.len().min(12)],
            &self.cask_index[..self.cask_index.len().min(12)],
        );
        let top = |out: &mut String, tally: &Tally| {
            for (why, all, sole) in tally.ranked().into_iter().take(10) {
                if sole == all {
                    out.push_str(&format!("   {why}: {all}\n"));
                } else {
                    out.push_str(&format!("   {why}: {all} ({sole} alone)\n"));
                }
            }
        };
        out.push_str(&format!(
            "formulae: {}/{} solo ({:.1}%), {} with closure ({:.1}%)\n",
            self.solo.admitted.len(),
            self.formulae,
            percent(self.solo.admitted.len(), self.formulae),
            self.closure.admitted.len(),
            percent(self.closure.admitted.len(), self.formulae),
        ));
        top(&mut out, &self.solo);
        out.push_str(&format!(
            "  admitted solo, refused with closure: {}\n",
            self.closure.refused
        ));
        top(&mut out, &self.closure);
        out.push_str(&format!(
            "casks: {}/{} ({:.1}%)\n",
            self.cask.admitted.len(),
            self.casks,
            percent(self.cask.admitted.len(), self.casks),
        ));
        top(&mut out, &self.cask);
        out
    }

    /// Serialize the full census and ranked refusals as a JSON document.
    #[must_use]
    pub fn json(&self) -> Value {
        aterm_json::json!({
            "machine": self.machine,
            "indexes": {"formula.json": self.formula_index, "cask.json": self.cask_index},
            "formulae": {
                "total": self.formulae,
                "solo": self.solo.admitted.len(),
                "closure": self.closure.admitted.len(),
                "refusals": self.solo.ranked(),
                "closure_refusals": self.closure.ranked(),
                "solo_names": self.solo.admitted,
                "closure_names": self.closure.admitted,
            },
            "casks": {
                "total": self.casks,
                "admitted": self.cask.admitted.len(),
                "refusals": self.cask.ranked(),
                "admitted_names": self.cask.admitted,
            },
        })
    }
}

#[cfg(test)]
#[path = "census_tests.rs"]
mod tests;
