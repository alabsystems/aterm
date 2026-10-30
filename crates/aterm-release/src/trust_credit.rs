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
//! A calculus row is not `Certified`: Trust reports it `Trusted` until the owner's E4.7 ruling,
//! and this counter is separate from any `Certified` total.
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
        let f: Vec<&str> = line.split('\t').collect();
        let [id, old, mapped, floor, why] = f[..] else {
            return Err(format!("{EPOCHS}:{n}: {} fields, not 5", f.len()));
        };
        let id: u32 = id
            .parse()
            .map_err(|_| format!("{EPOCHS}:{n}: epoch {id:?} is not a number"))?;
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
        let calculus_floor: u64 = floor
            .parse()
            .map_err(|_| format!("{EPOCHS}:{n}: floor {floor:?} is not a count"))?;
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
        if let Some(first) = keys.insert(key.clone(), n) {
            return Err(format!(
                "line {n}: key {key:?} was written on line {first} too, and Trust writes one line per key"
            ));
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
    let mut at = Vec::with_capacity(p.len());
    for premise in p {
        let Some(i) = PREMISES.iter().position(|known| known == premise) else {
            return Err(format!("unknown grade premise {premise:?}"));
        };
        at.push(i);
    }
    if !at.windows(2).all(|pair| pair[0] < pair[1]) {
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
/// [`CALCULUS`], discharge [`CALCULUS_DISCHARGE`], and every one of [`CALCULUS_PREMISES`]
/// stated. Only such a proved row is "proved (calculus)".
#[must_use]
pub fn is_calculus_row(encoding: &str, discharge: &str, premises: &[String]) -> bool {
    encoding == CALCULUS
        && discharge == CALCULUS_DISCHARGE
        && CALCULUS_PREMISES
            .iter()
            .all(|want| premises.iter().any(|p| p == want))
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
        let mut s = format!("proved (calculus): {} of {} proved [", t.calculus, t.proved);
        match epoch {
            Some(e) => s.push_str(&format!(
                "credit epoch {}, floor {}; {grading}",
                e.id, e.calculus_floor
            )),
            None => s.push_str(&format!("no credit epoch; {grading}")),
        }
        if !self.absent.is_empty() {
            s.push_str(&format!("; no record: {}", self.absent.join(", ")));
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
    match std::fs::read_to_string(repo.join(EPOCHS)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{EPOCHS}: {e}")),
        Ok(text) => Ok(parse_epochs(&text)?.pop()),
    }
}

/// Read one library's record from its check's target directory: `None` when there is none.
#[must_use]
pub fn read_record(target: &Path, package: &str) -> Option<Result<Tally, String>> {
    let path = record_path(target, package);
    match std::fs::read_to_string(&path) {
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
        let read = match std::fs::read_to_string(r) {
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
mod tests {
    use super::*;

    const HEADER: &str = r#"{"schema":"trust.obligations.v1","package":"alpha","lane":"advisory"}"#;

    /// A record of `lines`, the n-th line's key made `alpha::f#n` (Trust writes one line per
    /// key).
    fn record(lines: &[&str]) -> String {
        let keyed: Vec<String> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| l.replacen("\"alpha::f#1\"", &format!("\"alpha::f#{}\"", i + 1), 1))
            .collect();
        record_raw(&keyed.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// A record of `lines` exactly as given.
    fn record_raw(lines: &[&str]) -> String {
        let mut s = format!("{HEADER}\n{{\"fn\":\"alpha::f\",\"source\":null}}\n");
        for l in lines {
            s.push_str(l);
            s.push('\n');
        }
        s
    }

    /// An obligation line as targo 0.11.0 writes it: no grade fields.
    fn old(status: &str) -> String {
        format!(
            r#"{{"key":"alpha::f#1","kind":"overflow","status":"{status}","stop":null,"file":"src/lib.rs","line":1,"col":0}}"#
        )
    }

    /// An obligation line as a grading targo writes it.
    fn graded(status: &str, encoding: &str, discharge: &str) -> String {
        graded_with(status, encoding, discharge, &[])
    }

    /// An obligation line with premises.
    fn graded_with(status: &str, encoding: &str, discharge: &str, premises: &[&str]) -> String {
        let premises = premises
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"col":0,"discharge":"{discharge}","encoding":"{encoding}","file":"src/lib.rs","key":"alpha::f#1","kind":"overflow","line":1,"premises":[{premises}],"status":"{status}","stop":null}}"#
        )
    }

    /// A row Trust's VF5 calculus authority (`CalculusRowProved`) graded, as targo-trust writes
    /// it.
    fn calculus(status: &str) -> String {
        graded_with(status, CALCULUS, CALCULUS_DISCHARGE, &CALCULUS_PREMISES)
    }

    fn epoch(floor: u64) -> Epoch {
        Epoch {
            id: 1,
            old: OLD_PROVED.into(),
            mapped: "unvalidated-encoder".into(),
            calculus_floor: floor,
            why: "test".into(),
        }
    }

    #[test]
    fn the_committed_ledger_starts_the_counter_at_zero() {
        let text = include_str!("../trust/credit.tsv");
        let epochs = parse_epochs(text).expect("the committed ledger parses");
        let first = &epochs[0];
        assert_eq!(first.id, 1);
        assert_eq!(first.old, OLD_PROVED);
        assert_eq!(first.mapped, "unvalidated-encoder");
        assert_eq!(first.calculus_floor, 0, "proved (calculus) starts at 0");
    }

    #[test]
    fn an_ungraded_record_counts_zero_and_says_ungraded() {
        let text = record(&[&old("proved"), &old("proved"), &old("unknown")]);
        let t = tally(&text).unwrap();
        assert_eq!(
            t,
            Tally {
                proved: 2,
                calculus: 0,
                graded: 0
            }
        );
        let mut c = Credit::default();
        c.add("alpha", Some(Ok(t)));
        let line = c.line(Some(&epoch(0)));
        assert_eq!(
            line,
            "proved (calculus): 0 of 2 proved [credit epoch 1, floor 0; ungraded: the prover stated no grade on any of 2 proved]"
        );
        assert!(judge(&epoch(0), &c).is_empty(), "0 holds a floor of 0");
        // Negative control: the same ungraded record fails a floor above 0, since an
        // ungraded proof is no calculus credit.
        let failed = judge(&epoch(1), &c);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].contains("CREDIT-REGRESSED 0"), "{failed:?}");
    }

    #[test]
    fn written_ungraded_rows_count_zero() {
        let text = record(&[&graded("proved", UNGRADED, UNGRADED)]);
        assert_eq!(
            tally(&text).unwrap(),
            Tally {
                proved: 1,
                calculus: 0,
                graded: 0
            }
        );
    }

    #[test]
    fn calculus_counts_only_proven_calculus_proofs() {
        let text = record(&[
            &graded("proved", "unvalidated-encoder", "kernel-reconstructed"),
            &graded("proved", "e6-reflector", "kernel-reconstructed"),
            // Trust's KR0 switch: the Certified rows' discharge, read as graded, not calculus.
            &graded("proved", "unvalidated-encoder", "kernel-reference-checked"),
            &graded("proved", "e6-reflector", "kernel-reference-checked"),
            &graded("unknown", "not-encoded", "assumed"),
            &old("failed"),
        ]);
        let t = tally(&text).unwrap();
        assert_eq!(
            t,
            Tally {
                proved: 4,
                calculus: 0,
                graded: 4
            }
        );
        // Negative control: a discharge Trust does not state is still refused.
        let text = record(&[&graded(
            "proved",
            "e6-reflector",
            "kernel-reference-guessed",
        )]);
        assert!(tally(&text).unwrap_err().contains("unknown discharge"));
        // Negative control: one row graded as Trust's calculus authority grades it is
        // counted, and only when proved.
        let text = record(&[
            &calculus("proved"),
            &graded("failed", UNGRADED, UNGRADED),
            &graded("proved", "unvalidated-encoder", "solver-asserted"),
        ]);
        let t = tally(&text).unwrap();
        assert_eq!(
            t,
            Tally {
                proved: 2,
                calculus: 1,
                graded: 2
            }
        );
        let mut c = Credit::default();
        c.add("alpha", Some(Ok(t)));
        assert!(judge(&epoch(1), &c).is_empty());
        assert_eq!(
            c.line(Some(&epoch(1))),
            "proved (calculus): 1 of 2 proved [credit epoch 1, floor 1; graded]"
        );
    }

    /// Trust VF5: the counter reads the calculus authority's grade off the record, as
    /// targo-trust writes `CalculusRowProved`'s rows, and nothing else as calculus. A
    /// `proven-calculus` line with another discharge, or with a premise dropped, is refused:
    /// Trust states no such grade, so it is a record nobody can read.
    #[test]
    fn the_counter_reads_the_vf5_calculus_authority_from_the_record() {
        let text = record(&[
            &calculus("proved"),
            &calculus("proved"),
            &graded("proved", "unvalidated-encoder", "kernel-reference-checked"),
            &graded("unknown", UNGRADED, UNGRADED),
        ]);
        let t = tally(&text).unwrap();
        assert_eq!(
            t,
            Tally {
                proved: 3,
                calculus: 2,
                graded: 3
            }
        );
        let mut c = Credit::default();
        c.add("alpha", Some(Ok(t)));
        assert_eq!(
            c.line(Some(&epoch(0))),
            "proved (calculus): 2 of 3 proved [credit epoch 1, floor 0; graded]"
        );
        assert!(judge(&epoch(2), &c).is_empty(), "2 holds a floor of 2");
        assert_eq!(judge(&epoch(3), &c).len(), 1, "and fails a floor of 3");

        for bad in [
            graded_with(
                "proved",
                CALCULUS,
                "kernel-reconstructed",
                &CALCULUS_PREMISES,
            ),
            graded_with(
                "proved",
                CALCULUS,
                CALCULUS_DISCHARGE,
                &["semr-translation", "target-kappa"],
            ),
            graded("proved", CALCULUS, CALCULUS_DISCHARGE),
        ] {
            let why = tally(&record(&[&bad])).unwrap_err();
            assert!(
                why.contains("calculus authority") || why.contains("no proof authority"),
                "{why}"
            );
        }
        assert!(is_calculus_row(
            CALCULUS,
            CALCULUS_DISCHARGE,
            &CALCULUS_PREMISES.map(String::from)
        ));
        assert!(!is_calculus_row(
            "unvalidated-encoder",
            CALCULUS_DISCHARGE,
            &CALCULUS_PREMISES.map(String::from)
        ));
    }

    /// Review finding 4: a line Trust neither writes nor reads back is refused, not counted.
    /// One line per key; premises in Trust's declaration order, none repeated, each a premise
    /// Trust knows; an (encoding, discharge) pair some authority states; and a grade only on a
    /// status that admits it. Negative controls beside each: the same record as Trust writes it
    /// counts.
    #[test]
    fn a_line_trust_would_not_write_is_refused() {
        // The same calculus line five times under one key: refused, not 5.
        let line = calculus("proved");
        let five = record_raw(&[&line, &line, &line, &line, &line]);
        assert!(tally(&five).unwrap_err().contains("one line per key"));
        let keyed = record(&[&line, &line, &line, &line, &line]);
        assert_eq!(
            tally(&keyed).unwrap().calculus,
            5,
            "control: five keys count five"
        );

        // Premises reordered, repeated, or unknown.
        for (premises, want) in [
            (
                &["closed-world-entry", "semr-translation", "target-kappa"][..],
                "declaration order",
            ),
            (
                &[
                    "target-kappa",
                    "semr-translation",
                    "closed-world-entry",
                    "closed-world-entry",
                ][..],
                "declaration order",
            ),
            (
                &[
                    "semr-translation",
                    "target-kappa",
                    "closed-world-entry",
                    "closed-world-entry",
                ][..],
                "declaration order",
            ),
            (
                &[
                    "semr-translation",
                    "target-kappa",
                    "closed-world-entry",
                    "made-up",
                ][..],
                "unknown grade premise",
            ),
        ] {
            let bad = graded_with("proved", CALCULUS, CALCULUS_DISCHARGE, premises);
            let why = tally(&record(&[&bad])).unwrap_err();
            assert!(why.contains(want), "{premises:?}: {why}");
        }
        assert_eq!(
            tally(&record(&[&calculus("proved")])).unwrap().calculus,
            1,
            "control: the authority's premises in order"
        );

        // A grade on a status that does not admit it.
        for status in ["failed", "unknown", "timeout"] {
            let why = tally(&record(&[&calculus(status)])).unwrap_err();
            assert!(why.contains("cannot carry"), "{status}: {why}");
            let kr0 = graded(status, "e6-reflector", "kernel-reference-checked");
            assert!(
                tally(&record(&[&kr0]))
                    .unwrap_err()
                    .contains("cannot carry")
            );
        }
        let assumed = graded("unknown", "not-encoded", "assumed");
        assert_eq!(
            tally(&record(&[&assumed])).unwrap().proved,
            0,
            "control: an unknown row may carry an assumed grade"
        );

        // A pair no authority states.
        let pair = graded("proved", "e6-reflector", "solver-asserted");
        assert!(
            tally(&record(&[&pair]))
                .unwrap_err()
                .contains("no proof authority")
        );
        let ok = graded("proved", "unvalidated-encoder", "solver-asserted");
        assert_eq!(
            tally(&record(&[&ok])).unwrap().graded,
            1,
            "control: an admitted pair"
        );
    }

    #[test]
    fn a_grade_nobody_can_read_is_refused_not_ungraded() {
        let half = r#"{"key":"alpha::f#1","status":"proved","encoding":"proven-calculus"}"#;
        assert!(tally(&record(&[half])).unwrap_err().contains("together"));
        let unknown = graded("proved", "proven-by-vibes", "assumed");
        assert!(
            tally(&record(&[&unknown]))
                .unwrap_err()
                .contains("unknown encoding")
        );
        let lopsided = graded("proved", UNGRADED, "assumed");
        assert!(
            tally(&record(&[&lopsided]))
                .unwrap_err()
                .contains("ungraded row")
        );
        let other = record(&[]).replace("trust.obligations.v1", "trust.obligations.v0");
        assert!(tally(&other).unwrap_err().contains("schema"));
        let mut c = Credit::default();
        c.add("alpha", Some(tally(&record(&[half]))));
        let failed = judge(&epoch(0), &c);
        assert!(
            failed[0].starts_with("alpha CREDIT-UNREADABLE"),
            "{failed:?}"
        );
    }

    #[test]
    fn a_check_that_left_no_record_is_named_and_counts_zero() {
        let mut c = Credit::default();
        c.add("alpha", None);
        assert_eq!(
            c.line(Some(&epoch(0))),
            "proved (calculus): 0 of 0 proved [credit epoch 1, floor 0; ungraded: no trust.obligations.v1 record; no record: alpha]"
        );
        assert!(judge(&epoch(0), &c).is_empty());
        assert_eq!(judge(&epoch(2), &c).len(), 1);
    }

    #[test]
    fn the_ledger_moves_forward_only() {
        let row = |id: u32, floor: u64| {
            let old = if id == 1 {
                OLD_PROVED
            } else {
                "unvalidated-encoder"
            };
            format!("{id}\t{old}\tunvalidated-encoder\t{floor}\twhy\n")
        };
        // A later epoch maps an encoding, never the pre-grade `proved`.
        assert!(
            parse_epochs(&(row(1, 0) + "2\tproved\tunvalidated-encoder\t0\twhy\n"))
                .unwrap_err()
                .contains("later one")
        );
        assert_eq!(parse_epochs(&(row(1, 0) + &row(2, 3))).unwrap().len(), 2);
        assert!(
            parse_epochs(&(row(1, 3) + &row(2, 2)))
                .unwrap_err()
                .contains("lowers")
        );
        assert!(
            parse_epochs(&(row(1, 0) + &row(3, 0)))
                .unwrap_err()
                .contains("follows")
        );
        assert!(
            parse_epochs("1\tproved\tsolver-said\t0\twhy\n")
                .unwrap_err()
                .contains("no encoding")
        );
        assert!(parse_epochs("1\tproved\tnot-encoded\t0\t \n").is_err());
        assert!(
            parse_epochs("1\tunvalidated-encoder\tunvalidated-encoder\t0\twhy\n")
                .unwrap_err()
                .contains("first epoch")
        );
        assert!(parse_epochs("# only a comment\n").is_err());
    }

    #[test]
    fn the_record_path_is_targo_trusts() {
        assert_eq!(
            record_path(Path::new("/t"), "aterm-json"),
            Path::new("/t/trust/obligations/aterm-json.ndjson")
        );
        assert_eq!(
            record_path(Path::new("/t"), "a.b"),
            Path::new("/t/trust/obligations/a_b.ndjson")
        );
    }
}
