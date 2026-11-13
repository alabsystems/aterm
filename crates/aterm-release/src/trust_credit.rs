// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The "proved (calculus)" counter and its credit-epoch ledger.
//!
//! Trust grades every passing obligation on three axes (`trust_types::authority_grade`):
//! `encoding` (where the checked proposition came from), `discharge` (who decided it) and
//! `premises`. Only `encoding = proven-calculus` (a VC calculus proved sound in the Clean
//! kernel) makes a proof a claim about the program. One proof authority states it: Trust's
//! VF5 `CalculusRowProved` (F1's calculus over SemR, `vc_sound_F1`/`compose_F1` admitted
//! through Clean's reference entry), always as discharge [`CALCULUS_DISCHARGE`] with the
//! premises [`CALCULUS_PREMISES`]; this counter counts exactly those rows ([`is_calculus_row`])
//! and refuses a `proven-calculus` line graded any other way. The grades ride `targo trust
//! check`'s per-package record ([`record_path`], read by [`tally`]), never the terminal log or
//! `report.ndjson`.
//!
//! A calculus row is not `Certified`: Trust reports it `Trusted` until Trust's decision E4.7 is
//! taken, and this counter is separate from any `Certified` total.
//!
//! The ledger ([`EPOCHS`], [`parse_epochs`]) is versioned rows, the last one current. A row
//! maps the grade a proof had before it to the grade it is credited at now, and holds the
//! floor [`judge`] guards: fewer calculus proofs than the current floor fails the run.
//!
//! A prover that states no grade (targo 0.11.0, for one) writes obligation lines without the
//! three fields, and its record reads as ungraded and counts 0 ([`Credit::line`]). A
//! half-graded line, and any line Trust's own `trust.obligations.v1` reader refuses or its
//! writer never writes (an unknown encoding, discharge or premise, premises out of order or
//! repeated, a pair no authority states, a grade on a status that does not admit it, a key
//! written twice), is refused: a grade nobody can read is not credit.
//!
//! What this cannot check: that Trust wrote the record. Trust's header carries no seal yet
//! (`"seal": {"absent": "A8"}`), so a file shaped like a record counts as one; the counter
//! refuses only what Trust would not write.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The committed ledger of credit epochs.
pub const EPOCHS: &str = "crates/aterm-release/trust/credit.tsv";
/// The record schema [`tally`] reads.
pub const RECORD_SCHEMA: &str = "trust.obligations.v1";
/// The encoding that earns calculus credit.
pub const CALCULUS: &str = "proven-calculus";
/// The one discharge Trust's calculus authority (`CalculusRowProved`) states: Clean's fenced
/// reference entry accepted the row's `RowProved` judgment.
pub const CALCULUS_DISCHARGE: &str = "kernel-reference-checked";
/// The premises Trust's calculus authority states, in Trust's declaration order
/// (`trust_types::authority_grade::CALCULUS_ROW_PREMISES`): SemR's reading of the artifact,
/// the target κ, and the closed-world entry.
pub const CALCULUS_PREMISES: [&str; 3] = ["semr-translation", "target-kappa", "closed-world-entry"];
/// A row no proof authority graded.
pub const UNGRADED: &str = "ungraded";
/// The grade of a proof before the first epoch: `proved`, with no axes.
pub const OLD_PROVED: &str = "proved";
/// The encodings a compiler may state (`trust_types::authority_grade::GradeEncoding`).
const ENCODINGS: [&str; 4] = [
    CALCULUS,
    "unvalidated-encoder",
    "e6-reflector",
    "not-encoded",
];
/// The discharges a compiler may state (`GradeDischarge`). `kernel-reference-checked` is
/// Trust's KR0 switch: the Clean kernel checked the term AND its fenced Lean reference entry
/// accepted the same judgment (the two `Certified` variants). It is a discharge, not an
/// encoding, so it earns no calculus credit by itself.
const DISCHARGES: [&str; 6] = [
    "kernel-reference-checked",
    "kernel-reconstructed",
    "solver-self-checked",
    "solver-asserted",
    "compiler-composed",
    "assumed",
];

/// The premise classes a compiler may state, in Trust's declaration order
/// (`trust_types::authority_grade::GradePremise`): a written grade lists its premises in this
/// order with none repeated.
const PREMISES: [&str; 15] = [
    "parameter-view",
    "requires-link",
    "argument-premise",
    "type-invariant-certificate",
    "closed-world-callers",
    "panic-free-callee-registry",
    "sibling-rows",
    "body-proof-group",
    "requires-assumed-at-entry",
    "user-assume-total",
    "sibling-premises",
    "executor-protocol",
    "semr-translation",
    "target-kappa",
    "closed-world-entry",
];
/// The (encoding, discharge) pairs a compiler states (`trust_types::authority_grade::
/// ADMITTED_AXES`): a record naming any other pair is one Trust's reader refuses.
const ADMITTED_AXES: [(&str, &str); 9] = [
    (CALCULUS, CALCULUS_DISCHARGE),
    ("unvalidated-encoder", "kernel-reference-checked"),
    ("e6-reflector", "kernel-reference-checked"),
    ("unvalidated-encoder", "kernel-reconstructed"),
    ("unvalidated-encoder", "solver-self-checked"),
    ("unvalidated-encoder", "solver-asserted"),
    ("unvalidated-encoder", "compiler-composed"),
    ("e6-reflector", "kernel-reconstructed"),
    ("not-encoded", "assumed"),
];

/// One ledger row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Epoch {
    /// 1, 2, …: each row's is one more than the last.
    pub id: u32,
    /// The grade a proof carried before this epoch.
    pub old: String,
    /// The encoding that grade is credited at from this epoch on.
    pub mapped: String,
    /// The fewest "proved (calculus)" obligations a run may report.
    pub calculus_floor: u64,
    /// Why the epoch began.
    pub why: String,
}

/// Read the ledger: `#` lines are comments; each row is
/// `<epoch>\t<old grade>\t<mapped encoding>\t<proved (calculus) floor>\t<why>`.
///
/// # Errors
/// A row without five fields, an epoch that is not a number or not the one after the row
/// before, an old grade other than `proved` in the first row or an encoding in a later one, a
/// mapped grade that is not an encoding, a floor that is not a count, a floor lower than the
/// epoch before's, an empty reason, or no row at all.
pub fn parse_epochs(text: &str) -> Result<Vec<Epoch>, String> {
    let mut out: Vec<Epoch> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        let (Some(id), Some(old), Some(mapped), Some(floor), Some(why), None) = (
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
        ) else {
            return Err(format!(
                "{EPOCHS}:{n}: {} fields, not 5",
                line.split('\t').count()
            ));
        };
        let id: u32 = (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            .then(|| id.parse().ok())
            .flatten()
            .ok_or_else(|| format!("{EPOCHS}:{n}: epoch {id:?} is not a number"))?;
        let want = out.last().map_or(1, |e| e.id + 1);
        if id != want {
            return Err(format!(
                "{EPOCHS}:{n}: epoch {id} follows epoch {}",
                want - 1
            ));
        }
        let old_ok = if id == 1 {
            old == OLD_PROVED
        } else {
            ENCODINGS.contains(&old)
        };
        if !old_ok {
            return Err(format!(
                "{EPOCHS}:{n}: epoch {id} maps {old:?}: the first epoch maps {OLD_PROVED:?}, a later one an encoding"
            ));
        }
        if !ENCODINGS.contains(&mapped) {
            return Err(format!(
                "{EPOCHS}:{n}: {mapped:?} is no encoding ({})",
                ENCODINGS.join(", ")
            ));
        }
        let calculus_floor: u64 = (!floor.is_empty() && floor.bytes().all(|b| b.is_ascii_digit()))
            .then(|| floor.parse().ok())
            .flatten()
            .ok_or_else(|| format!("{EPOCHS}:{n}: floor {floor:?} is not a count"))?;
        if let Some(prev) = out.last()
            && calculus_floor < prev.calculus_floor
        {
            return Err(format!(
                "{EPOCHS}:{n}: epoch {id} lowers the proved (calculus) floor {} to {calculus_floor}",
                prev.calculus_floor
            ));
        }
        if why.trim().is_empty() {
            return Err(format!("{EPOCHS}:{n}: epoch {id} gives no reason"));
        }
        out.push(Epoch {
            id,
            old: old.to_string(),
            mapped: mapped.to_string(),
            calculus_floor,
            why: why.to_string(),
        });
    }
    if out.is_empty() {
        return Err(format!("{EPOCHS}: no epoch"));
    }
    Ok(out)
}

/// What one package's record says about its proved obligations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Obligation lines with status `proved`.
    pub proved: u64,
    /// Of those, the ones graded `proven-calculus`.
    pub calculus: u64,
    /// Of those, the ones carrying a grade at all.
    pub graded: u64,
}

impl Tally {
    fn add(&mut self, o: Tally) {
        self.proved += o.proved;
        self.calculus += o.calculus;
        self.graded += o.graded;
    }
}

/// `<target-dir>/trust/obligations/<package>.ndjson`, the package name made a file stem as
/// targo-trust makes it (`record_file_stem`).
#[must_use]
pub fn record_path(target: &Path, package: &str) -> PathBuf {
    let stem: String = package
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    target
        .join("trust")
        .join("obligations")
        .join(format!("{stem}.ndjson"))
}

#[derive(Deserialize)]
struct Line {
    schema: Option<String>,
    key: Option<String>,
    status: Option<String>,
    encoding: Option<String>,
    discharge: Option<String>,
    premises: Option<Vec<String>>,
}

/// Count one `trust.obligations.v1` record's proved obligations by grade. The header must
/// name the schema; `fn` lines are skipped; an obligation line with none of `encoding`,
/// `discharge`, `premises` is ungraded (a prover that states no grade). A graded line is held
/// to what Trust's own reader (`targo-trust` `check_grade_fields`) and writer allow
/// ([`check_grade`]), and a key may appear once: Trust writes one line per key (the worst of a
/// key's rows), so a record that repeats one is not Trust's.
///
/// # Errors
/// An empty record, another schema, a line that is not JSON, an obligation line with no
/// status, a key written twice, a line with some but not all of the three grade fields, or a
/// grade [`check_grade`] refuses.
pub fn tally(text: &str) -> Result<Tally, String> {
    let mut t = Tally::default();
    let mut lines = text.lines();
    let header: Line = aterm_json::from_str(lines.next().ok_or("the record is empty")?)
        .map_err(|e| format!("line 1 is not a record header: {e}"))?;
    match header.schema.as_deref() {
        Some(RECORD_SCHEMA) => {}
        other => {
            return Err(format!(
                "the header says schema {}, and this counter reads {RECORD_SCHEMA}",
                other.unwrap_or("(none)")
            ));
        }
    }
    let mut keys: BTreeMap<String, usize> = BTreeMap::new();
    for (i, raw) in lines.enumerate() {
        let n = i + 2;
        let l: Line =
            aterm_json::from_str(raw).map_err(|e| format!("line {n} is not JSON: {e}"))?;
        let Some(key) = l.key else {
            continue; // a function's `fn` line
        };
        match keys.entry(key) {
            std::collections::btree_map::Entry::Occupied(entry) => {
                return Err(format!(
                    "line {n}: key {:?} was written on line {} too, and Trust writes one line per key",
                    entry.key(),
                    entry.get()
                ));
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(n);
            }
        }
        let status = l
            .status
            .ok_or_else(|| format!("line {n}: an obligation with no status"))?;
        let grade = match (l.encoding, l.discharge, l.premises) {
            (None, None, None) => None,
            (Some(e), Some(d), Some(p)) => Some((e, d, p)),
            _ => {
                return Err(format!(
                    "line {n}: a grade needs encoding, discharge and premises together"
                ));
            }
        };
        let graded = match &grade {
            None => false,
            Some((e, d, p)) => {
                check_grade(&status, e, d, p).map_err(|why| format!("line {n}: {why}"))?
            }
        };
        if status != "proved" {
            continue;
        }
        t.proved += 1;
        if graded {
            t.graded += 1;
            if grade.is_some_and(|(e, d, p)| is_calculus_row(&e, &d, &p)) {
                t.calculus += 1;
            }
        }
    }
    Ok(t)
}

/// Whether one obligation line's grade is one Trust writes, as its reader
/// (`targo-trust` `check_grade_fields`, `ObligationGrade::admitted`) decides: `Ok(false)` for
/// the written `ungraded` triple, `Ok(true)` for a grade.
///
/// # Errors
/// An `ungraded` triple with another field or a premise; an encoding, discharge or premise
/// outside Trust's vocabulary; premises out of Trust's declaration order or repeated; an
/// (encoding, discharge) pair no proof authority states ([`ADMITTED_AXES`]); a
/// [`CALCULUS`] grade that is not the calculus authority's ([`is_calculus_row`]); or a grade
/// the status does not admit (a `proved` row any grade, an `unknown` row only an `assumed`
/// one, no other row any).
fn check_grade(status: &str, e: &str, d: &str, p: &[String]) -> Result<bool, String> {
    if e == UNGRADED || d == UNGRADED {
        if e != d || !p.is_empty() {
            return Err(format!(
                "an ungraded row is encoding and discharge {UNGRADED} with no premises"
            ));
        }
        return Ok(false);
    }
    if !ENCODINGS.contains(&e) {
        return Err(format!("unknown encoding {e:?}"));
    }
    if !DISCHARGES.contains(&d) {
        return Err(format!("unknown discharge {d:?}"));
    }
    let mut prev = None;
    let mut ordered = true;
    for premise in p {
        let Some(i) = PREMISES.iter().position(|known| known == premise) else {
            return Err(format!("unknown grade premise {premise:?}"));
        };
        ordered &= prev.is_none_or(|last| last < i);
        prev = Some(i);
    }
    if !ordered {
        return Err(format!(
            "grade premises {p:?} are not in Trust's declaration order without repeats"
        ));
    }
    if !ADMITTED_AXES.contains(&(e, d)) {
        return Err(format!(
            "no proof authority states encoding {e:?} with discharge {d:?}"
        ));
    }
    if e == CALCULUS && !is_calculus_row(e, d, p) {
        return Err(format!(
            "a {CALCULUS} grade is Trust's calculus authority's: discharge \
             {CALCULUS_DISCHARGE} with premises {}, not {d} with {p:?}",
            CALCULUS_PREMISES.join(", ")
        ));
    }
    let admits = match status {
        "proved" => true,
        "unknown" => d == "assumed",
        _ => false,
    };
    if !admits {
        return Err(format!("a {status:?} row cannot carry a {d:?} grade"));
    }
    Ok(true)
}

/// Whether a grade is Trust's calculus authority's (`CalculusRowProved`, VF5): encoding
/// [`CALCULUS`], discharge [`CALCULUS_DISCHARGE`], and [`CALCULUS_PREMISES`] stated in
/// declaration order with no extra or repeated premises. Only such a proved row is
/// "proved (calculus)".
#[must_use]
pub fn is_calculus_row(encoding: &str, discharge: &str, premises: &[String]) -> bool {
    encoding == CALCULUS
        && discharge == CALCULUS_DISCHARGE
        && premises.len() == CALCULUS_PREMISES.len()
        && premises
            .iter()
            .zip(CALCULUS_PREMISES)
            .all(|(p, want)| p == want)
}

/// What a run's records say, against the current epoch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credit {
    /// The sum over the libraries whose record was read.
    pub total: Tally,
    /// Libraries whose check left no record.
    pub absent: Vec<String>,
    /// Libraries whose record was refused, and why.
    pub refused: Vec<(String, String)>,
}

impl Credit {
    /// Fold one library's record: `None` when its check left none.
    pub fn add(&mut self, name: &str, record: Option<Result<Tally, String>>) {
        match record {
            None => self.absent.push(name.to_string()),
            Some(Ok(t)) => self.total.add(t),
            Some(Err(why)) => self.refused.push((name.to_string(), why)),
        }
    }

    /// The counter: `proved (calculus): N of P proved [credit epoch E, floor F; <grading>]`.
    #[must_use]
    pub fn line(&self, epoch: Option<&Epoch>) -> String {
        let t = self.total;
        let grading = if t.proved == 0 {
            if self.absent.is_empty() {
                "no proved obligation".to_string()
            } else {
                format!("ungraded: no {RECORD_SCHEMA} record")
            }
        } else if t.graded == 0 {
            format!(
                "ungraded: the prover stated no grade on any of {} proved",
                t.proved
            )
        } else if t.graded < t.proved {
            format!("partly graded: {} of {} proved", t.graded, t.proved)
        } else {
            "graded".to_string()
        };
        use std::fmt::Write as _;
        let mut s = format!("proved (calculus): {} of {} proved [", t.calculus, t.proved);
        match epoch {
            Some(e) => {
                let _ = write!(
                    s,
                    "credit epoch {}, floor {}; {grading}",
                    e.id, e.calculus_floor
                );
            }
            None => {
                let _ = write!(s, "no credit epoch; {grading}");
            }
        }
        if !self.absent.is_empty() {
            let _ = write!(s, "; no record: {}", self.absent.join(", "));
        }
        s.push(']');
        s
    }
}

/// The current epoch's verdict on `credit`: each line a failure. Fewer calculus proofs
/// than the floor, and every refused record, fail; an ungraded or absent record counts 0,
/// so it fails only a floor above 0.
#[must_use]
pub fn judge(epoch: &Epoch, credit: &Credit) -> Vec<String> {
    let mut out: Vec<String> = credit
        .refused
        .iter()
        .map(|(name, why)| format!("{name} CREDIT-UNREADABLE {why}"))
        .collect();
    if credit.total.calculus < epoch.calculus_floor {
        out.push(format!(
            "proved (calculus) CREDIT-REGRESSED {} below credit epoch {}'s floor {}",
            credit.total.calculus, epoch.id, epoch.calculus_floor
        ));
    }
    out
}

/// The ledger's current epoch in `repo`: `Ok(None)` with no ledger.
///
/// # Errors
/// The ledger is there and does not parse.
pub fn current_epoch(repo: &Path) -> Result<Option<Epoch>, String> {
    match crate::durable::read_text(&repo.join(EPOCHS)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{EPOCHS}: {e}")),
        Ok(text) => Ok(parse_epochs(&text)?.pop()),
    }
}

/// Read one library's record from its check's target directory: `None` when there is none.
#[must_use]
pub fn read_record(target: &Path, package: &str) -> Option<Result<Tally, String>> {
    let path = record_path(target, package);
    match crate::durable::read_text(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(format!("{}: {e}", path.display()))),
        Ok(text) => Some(tally(&text).map_err(|why| format!("{}: {why}", path.display()))),
    }
}

/// `ship trust credit <record>…`: the counter over the records named, judged against the
/// working ledger. 0 holds, 1 regressed or a record refused, 2 the ledger missing or
/// unreadable.
pub fn run_cmd(repo: &Path, records: &[PathBuf]) -> i32 {
    let epoch = match current_epoch(repo) {
        Ok(Some(e)) => e,
        Ok(None) => {
            eprintln!("aterm-release trust credit: no {EPOCHS}");
            return 2;
        }
        Err(why) => {
            eprintln!("aterm-release trust credit: {why}");
            return 2;
        }
    };
    let mut credit = Credit::default();
    let mut by_name: BTreeMap<String, Option<Result<Tally, String>>> = BTreeMap::new();
    for r in records {
        let read = match crate::durable::read_text(r) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => Some(Err(e.to_string())),
            Ok(text) => Some(tally(&text)),
        };
        by_name.insert(r.display().to_string(), read);
    }
    for (name, read) in by_name {
        credit.add(&name, read);
    }
    println!("{}", credit.line(Some(&epoch)));
    let failed = judge(&epoch, &credit);
    for f in &failed {
        println!("{f}");
    }
    i32::from(!failed.is_empty())
}

#[cfg(test)]
#[path = "trust_credit_tests.rs"]
mod tests;
