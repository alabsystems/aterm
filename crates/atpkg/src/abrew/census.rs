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
mod tests {
    use super::*;

    const SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    /// A bottled formula record; `cellar` decides relocation.
    fn formula(name: &str, cellar: &str, deps: &[&str]) -> Value {
        aterm_json::json!({
            "name": name, "full_name": name, "versions": {"stable": "1.0"}, "revision": 0,
            "dependencies": deps,
            "bottle": {"stable": {"files": {"arm64_tahoe": {"url": "https://x/b.tar.gz", "sha256": SHA, "cellar": cellar}}}},
        })
    }

    /// A cask record downloading `file`, placing one app.
    fn cask(token: &str, file: &str) -> Value {
        aterm_json::json!({
            "token": token, "version": "1.0", "url": format!("https://x/{file}"), "sha256": SHA,
            "artifacts": [{"app": ["Demo.app"]}],
        })
    }

    fn with(mut record: Value, key: &str, value: Value) -> Value {
        record
            .as_object_mut()
            .unwrap()
            .insert(key.to_string(), value);
        record
    }

    fn bytes(records: Vec<Value>) -> Vec<u8> {
        aterm_json::to_vec(&Value::Array(records)).unwrap()
    }

    /// The fixture: relocation-free `zlib-ng` and `tool` (which needs
    /// `zlib-ng`), relocating `openssl` and `curl-ish` (relocation-free, but
    /// needs `openssl`), a disabled formula, one asking for a newer macOS
    /// than the machine runs, relocating `libgit`, and relocation-free
    /// `client`, which needs both relocating formulae; a zip cask, a dmg
    /// cask, a pkg cask, a zip cask that depends on the relocating formula,
    /// and a zip cask that needs macOS 14 or newer.
    fn fixture() -> (Vec<u8>, Vec<u8>) {
        let formulae = vec![
            formula("zlib-ng", ":any_skip_relocation", &[]),
            formula("tool", ":any_skip_relocation", &["zlib-ng"]),
            formula("openssl", "/opt/homebrew/Cellar", &[]),
            formula("curl-ish", ":any_skip_relocation", &["openssl"]),
            with(
                formula("old", ":any_skip_relocation", &[]),
                "disabled",
                Value::Bool(true),
            ),
            with(
                formula("future", ":any_skip_relocation", &[]),
                "requirements",
                aterm_json::json!([{"name": "macos", "version": "30", "contexts": [], "specs": ["stable"]}]),
            ),
            formula("libgit", "/opt/homebrew/Cellar", &[]),
            formula("client", ":any_skip_relocation", &["openssl", "libgit"]),
        ];
        let casks = vec![
            cask("zipped", "Demo.zip"),
            cask("imaged", "Demo.dmg"),
            with(
                cask("installer", "Setup.zip"),
                "artifacts",
                aterm_json::json!([{"pkg": ["Setup.pkg"]}]),
            ),
            with(
                cask("needs-openssl", "Needs.zip"),
                "depends_on",
                aterm_json::json!({"formula": ["openssl"]}),
            ),
            with(
                cask("floored", "Floored.zip"),
                "depends_on",
                aterm_json::json!({"macos": {">=": ["14"]}}),
            ),
        ];
        (bytes(formulae), bytes(casks))
    }

    fn tahoe() -> Machine {
        Machine::mac("arm64", "26.1", Some("26.1"), true)
    }

    #[test]
    fn the_census_counts_what_admission_decides() {
        let (formulae, casks) = fixture();
        let census = measure(&formulae, &casks, &tahoe()).unwrap();
        assert_eq!((census.formulae, census.casks), (8, 5));
        assert_eq!(
            census.solo.admitted,
            ["zlib-ng", "tool", "curl-ish", "client"]
        );
        assert_eq!(
            census.solo.refusals,
            BTreeMap::from([
                ("<name> needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar)".to_string(), (2, 2)),
                ("<name> needs a newer macOS".to_string(), (1, 1)),
                ("abrew: <name> is disabled upstream".to_string(), (1, 1)),
            ])
        );
        assert_eq!(census.closure.admitted, ["zlib-ng", "tool"]);
        assert_eq!(
            census.closure.refusals,
            BTreeMap::from([
                (
                    "openssl needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar)".to_string(),
                    (2, 1)
                ),
                (
                    "libgit needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar)".to_string(),
                    (1, 0)
                ),
            ])
        );
        assert_eq!(
            census.solo.admitted.len(),
            census.closure.admitted.len() + census.closure.refused
        );
        assert_eq!(census.cask.admitted, ["zipped", "imaged", "floored"]);
        assert_eq!(
            census.cask.ranked(),
            [
                (
                    "cask <name> ships a pkg installer (root work abrew never does itself)",
                    1,
                    1
                ),
                (
                    "openssl needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar)",
                    1,
                    1
                ),
            ]
        );
        let text = census.text();
        assert!(
            text.contains("formulae: 4/8 solo (50.0%), 2 with closure (25.0%)\n"),
            "{text}"
        );
        assert!(
            text.contains("  admitted solo, refused with closure: 2\n   openssl needs its Homebrew build/relocation recipe (its bottle relocates: cellar /opt/homebrew/Cellar): 2 (1 alone)\n"),
            "{text}"
        );
        assert!(text.contains("casks: 3/5 (60.0%)\n"), "{text}");
        let json = census.json();
        assert_eq!(
            json.pointer("/formulae/closure").and_then(Value::as_u64),
            Some(2)
        );
        assert_eq!(
            json.pointer("/casks/admitted").and_then(Value::as_u64),
            Some(3)
        );
    }

    /// Negative control: the same records judged for other machines get
    /// other verdicts, so the counts above are admission's decisions for
    /// the machine modelled, not the fixture's shape. Big Sur has no bottle
    /// the relocation-free formulae were built for and is older than
    /// `floored`'s floor; macOS 30 meets `future`'s.
    #[test]
    fn the_census_follows_the_machine_it_models() {
        let (formulae, casks) = fixture();
        let big_sur = measure(
            &formulae,
            &casks,
            &Machine::mac("arm64", "11.7", None, false),
        )
        .unwrap();
        assert!(big_sur.solo.admitted.is_empty() && big_sur.closure.admitted.is_empty());
        assert_eq!(
            big_sur.solo.refusals["<name> needs its Homebrew build/relocation recipe (no bottle for this machine)"],
            (7, 7)
        );
        let next = measure(
            &formulae,
            &casks,
            &Machine::mac("arm64", "30.0", None, false),
        )
        .unwrap();
        assert_eq!(
            next.solo.admitted,
            ["zlib-ng", "tool", "curl-ish", "future", "client"]
        );
        assert_eq!(next.closure.admitted, ["zlib-ng", "tool", "future"]);
        assert!(
            !next
                .solo
                .refusals
                .contains_key("<name> needs a newer macOS")
        );
        assert_eq!(big_sur.cask.admitted, ["zipped", "imaged"]);
        assert_eq!(
            big_sur.cask.refusals["cask <name> needs macOS >= 14"],
            (1, 1)
        );
        assert_eq!(next.cask.admitted, ["zipped", "imaged", "floored"]);
    }

    #[test]
    fn what_is_not_an_index_is_refused() {
        let (formulae, casks) = fixture();
        assert!(measure(b"{}", &casks, &tahoe()).is_err());
        assert!(measure(&formulae, b"not json", &tahoe()).is_err());
        let empty = measure(b"[]", b"[]", &tahoe()).unwrap();
        assert!(
            empty.text().contains("formulae: 0/0 solo (0.0%)"),
            "{}",
            empty.text()
        );
    }

    #[test]
    fn only_the_refused_package_s_own_name_is_masked() {
        assert_eq!(
            masked("cask zip needs zip-tools.", "zip"),
            "cask <name> needs zip-tools."
        );
        assert_eq!(
            masked("abrew: python@3.13 is disabled upstream", "python@3.13"),
            "abrew: <name> is disabled upstream"
        );
        assert_eq!(masked("ends with go.", "go"), "ends with <name>.");
        assert_eq!(masked("a ago go-lang", "go"), "a ago go-lang");
        assert_eq!(masked("", "go"), "");
        assert_eq!(
            masked("cask installer ships a pkg installer", "installer"),
            "cask <name> ships a pkg installer"
        );
    }
}
