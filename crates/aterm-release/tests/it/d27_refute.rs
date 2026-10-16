// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D27 adversarial refutation: attacks on the claim "a candidate that loses any proved
//! obligation, other than an audited retraction applicable to that candidate, is never
//! admitted".
//!
//! Each test drives the real gate end to end where it can: `trust_compare::run` (behind
//! `aterm-release trust compare`) over two run directories laid out as the lane leaves them
//! (`.plan`, `<spec>.log`, `.rc`, `.oblig.tsv`), the real ledger parser, and real `git` (the
//! gate's `TrustGit`) in a real trust repository; records come from the real reduction
//! (`trust_oblig::reduce`) where the attack is in the report's shape. Each `refute_*` test is
//! an attack the gate must refuse, each `holds_*` one it withstands.
//!
//! The defences: twins at one key are told apart only by what both runs report of them and are
//! otherwise held to their worst case; a retraction applies only to the obligation it names
//! (`nth/of`); a function reported twice is refused; the trust repository is read with replace
//! refs and grafts ignored, and a cut history refused; a fix holds only while the candidate
//! keeps its reproducer and no revert of it; the seal's commit is required; a count never
//! stands for obligations; and a row applies only in the aterm tree and at the site it was
//! audited at (ledger v3), the ledger itself excepted.

use std::fs;
use std::path::{Path, PathBuf};

use crate::trust_compare::{self, Args};
use crate::trust_oblig::{self, LEDGER_MAGIC, Obligation, REPORT_SCHEMA, Record, Span};

// --- the two runs, as the lane leaves them -----------------------------------

const SEAL: &str = "a519d423f47ff6fb19d505012f210f13ad39f2f6f31cb3c3af5aff508551d0b2";
const MAIN: &str = "61a997f0b92df5c78bb1ab075ca3044ceb871789c82c66db2743c03f9f537db1";
const COMMIT: &str = "36b2c3df36eac4971b8189da9967ab7cc442bbf2";
const TREE: &str = "0123456789abcdef0123456789abcdef01234567";
const STUB: &str = "targo 1.99.0-dev (stub)";
const DONE: &str = "Coverage: 10/10 eligible function bodies verified (complete)\n";
const ADD: &str = "arithmetic_overflow_add";

fn tmp(prefix: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .unwrap()
}

/// A whole-tree plan of one library, `alpha`, of aterm [`COMMIT`].
fn plan(prover: &str, driver_id: &str) -> String {
    plan_at(prover, driver_id, COMMIT)
}

/// [`plan`], of aterm `commit`.
fn plan_at(prover: &str, driver_id: &str, commit: &str) -> String {
    let mut t = String::from("aterm-trust plan 1\n");
    t.push_str(&format!(
        "driver /t/bin/targo\ndriver-id {driver_id}\nonly \nper-commit 0\n"
    ));
    t.push_str("logdir /gone\nhost-mem 25769803776\nhost-mem-os -\nnow 2026-10-03T00:00:00Z\n");
    t.push_str("crate-budget 21600\nobligations 1\n");
    t.push_str("crate alpha\talpha@1.0.0\tgate\n");
    t.push_str("trustd /tmp/none.sock\nfile \nstore /s\n");
    t.push_str(&format!("commit {commit}\ntree {TREE}\nprover {prover}\n"));
    t.push_str("host 14 cores, 24 GiB\njobs 1\n");
    t
}

fn summary(p: u64, t: u64) -> String {
    format!(
        "Summary: {p} proved, 0 failed, 0 runtime-checked, 0 assumed, 0 mandated, 0 contract-panic, 0 inconclusive ({t} total); not-reproducible\n{DONE}"
    )
}

/// One run: alpha's check finished with `rec`'s counts, and left `rec`.
fn run_dir(prover: &str, driver_id: &str, rec: &Record) -> aterm_tempfile::TempDir {
    run_dir_at(prover, driver_id, rec, COMMIT)
}

/// [`run_dir`], a run of aterm `commit`.
fn run_dir_at(
    prover: &str,
    driver_id: &str,
    rec: &Record,
    commit: &str,
) -> aterm_tempfile::TempDir {
    let d = tmp("d27-refute-run");
    fs::write(d.path().join(".plan"), plan_at(prover, driver_id, commit)).unwrap();
    fs::write(
        d.path().join("alpha@1.0.0.log"),
        summary(rec.proved(), rec.total()),
    )
    .unwrap();
    fs::write(d.path().join("alpha@1.0.0.log.rc"), "0").unwrap();
    fs::write(d.path().join("alpha@1.0.0.log.oblig.tsv"), rec.to_text()).unwrap();
    d
}

#[derive(Default)]
struct Gate<'a> {
    ledger_rows: Vec<String>,
    /// A ledger already written (in an aterm repository), instead of `ledger_rows`.
    ledger_at: Option<&'a Path>,
    trust: Option<&'a Path>,
    base_commit: Option<&'a str>,
    cand_commit: Option<&'a str>,
    ratchet: Option<String>,
}

/// `aterm-release trust compare` over the two runs as the candidate gate calls it, the ledger
/// always given. Exit 0 PUBLISH-OK, 1 refused, 2 COULD NOT COMPARE; the report goes to stdout.
fn gate(base: &aterm_tempfile::TempDir, cand: &aterm_tempfile::TempDir, g: &Gate<'_>) -> i32 {
    let side = tmp("d27-refute-ledger");
    let ledger = match g.ledger_at {
        Some(p) => p.to_path_buf(),
        None => {
            let p = side.path().join("retractions.tsv");
            let mut text = format!("{LEDGER_MAGIC}\n");
            for r in &g.ledger_rows {
                text.push_str(r);
                text.push('\n');
            }
            fs::write(&p, text).unwrap();
            p
        }
    };
    let ratchet = g.ratchet.as_ref().map(|t| {
        let p = side.path().join("floors.tsv");
        fs::write(&p, t).unwrap();
        p
    });
    trust_compare::run(&Args {
        base: base.path().join(".plan"),
        candidate: cand.path().join(".plan"),
        retractions: Some(ledger),
        trust_repo: g.trust.map(Path::to_path_buf),
        base_trust_commit: g.base_commit.map(str::to_string),
        candidate_trust_commit: g.cand_commit.map(str::to_string),
        ratchet,
        ..Args::default()
    })
}

// --- records and reports ----------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn ob(
    function: &str,
    kind: &str,
    file: &str,
    (l0, c0, l1, c1): (u32, u32, u32, u32),
    status: &str,
    anchor: &str,
    desc: &str,
) -> Obligation {
    Obligation {
        function: function.into(),
        kind: kind.into(),
        file: file.into(),
        span: Span { l0, c0, l1, c1 },
        status: status.into(),
        anchor: anchor.into(),
        desc: desc.into(),
    }
}

fn record(rows: Vec<Obligation>) -> Record {
    Record {
        package: "alpha".into(),
        unit: "cargo-target(package=\"alpha\")".into(),
        functions: 1,
        rows,
    }
}

const UNIT: &str = r#"cargo-target(package_id=\"path+file:///x#alpha@1.0.0\",package=\"alpha\",kind=[\"lib\"],target=\"alpha\")"#;

/// (obligation_id, kind, status, file, l0, c0, l1, c1, description)
type O<'a> = (
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    u32,
    u32,
    u32,
    u32,
    &'a str,
);

fn function_line(path: &str, obls: &[O<'_>]) -> String {
    let proved = obls.iter().filter(|o| o.2 == "proved").count();
    let os: Vec<String> = obls
        .iter()
        .map(|(id, k, st, f, l0, c0, l1, c1, d)| {
            format!(
                r#"{{"obligation_id":"{id}","description":"{d}","kind":"{k}","location":{{"file":"{f}","line_start":{l0},"col_start":{c0},"line_end":{l1},"col_end":{c1}}},"outcome":{{"status":"{st}"}},"time_ms":0}}"#
            )
        })
        .collect();
    format!(
        r#"{{"record_type":"function","crate_name":"{UNIT}","function":"{UNIT}::{path}","summary":{{"total_obligations":{},"proved":{proved},"verdict":"x"}},"obligations":[{}]}}"#,
        obls.len(),
        os.join(",")
    )
}

/// A whole report: header, `functions`, and a footer that counts them.
fn report(functions: &[String]) -> String {
    let total: usize = functions
        .iter()
        .map(|f| f.matches("\"obligation_id\"").count())
        .sum();
    let proved: usize = functions
        .iter()
        .map(|f| f.matches(r#""status":"proved""#).count())
        .sum();
    let mut out = format!(
        r#"{{"record_type":"header","schema":"{REPORT_SCHEMA}","crate_name":"{UNIT}","expected_functions":{}}}"#,
        functions.len()
    );
    out.push('\n');
    for f in functions {
        out.push_str(f);
        out.push('\n');
    }
    out.push_str(&format!(
        r#"{{"record_type":"footer","schema":"{REPORT_SCHEMA}","summary":{{"total_obligations":{total},"total_proved":{proved},"verdict":"x"}},"functions_emitted":{}}}"#,
        functions.len()
    ));
    out.push('\n');
    out
}

/// A checkout whose `src/lib.rs` the in-tree spans below point into.
fn checkout() -> aterm_tempfile::TempDir {
    let d = tmp("d27-refute-checkout");
    fs::create_dir_all(d.path().join("src")).unwrap();
    fs::write(
        d.path().join("src/lib.rs"),
        concat!(
            "pub fn c(v: &[u8], i: usize) -> u8 {\n", // 1
            "    let f = |i: usize| v[i];\n",         // 2: `v[i]` at 23..27
            "    f(i)\n",                             // 3
            "}\n",                                    // 4
            "pub fn f(a: u32) -> u32 {\n",            // 5
            "    a + 1\n",                            // 6: `a + 1` at 4..9
            "}\n",                                    // 7
        ),
    )
    .unwrap();
    d
}

/// The lane's reduction of `text` (`trust_oblig::reduce`).
fn reduced(text: &str, repo: &Path) -> Result<Record, String> {
    let d = tmp("d27-refute-report");
    let p = d.path().join("report.ndjson");
    fs::write(&p, text).unwrap();
    trust_oblig::reduce(&p, "alpha", repo, None)
}

// --- a trust repository -----------------------------------------------------

const REPRO: &str = "crates/trust-vcgen/tests/false_proofs.rs";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = crate::ledger::testing::git()
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args([
            "-c",
            "maintenance.auto=false",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, text: &str, msg: &str) -> String {
    let p = dir.join(file);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

fn init(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
}

/// The fix commit's content: the reproducer the ledger names.
fn add_fix(dir: &Path) -> String {
    commit(
        dir,
        REPRO,
        "#[test]\nfn guard_by_reaching_def() {}\n",
        "fix: the false proof",
    )
}

/// The driver line of a run whose toolchain is trust `sha`.
fn driver_of(sha: &str) -> String {
    format!(
        "targo 1.99.0-dev ({} 2026-10-07) (targo 0.10.0+g{})",
        &sha[..9],
        &sha[..9]
    )
}

/// A ledger row (thirteen fields) audited at aterm [`COMMIT`], where the runs measure, against
/// the published seal at trust `seal`, which reported it at X's span, line 6, of `file`.
#[allow(clippy::too_many_arguments)]
fn row(
    function: &str,
    kind: &str,
    file: &str,
    anchor: &str,
    nth: &str,
    desc: &str,
    seal: &str,
    commit: &str,
) -> String {
    let site = format!("{file}:6:4-6:9");
    row_at(
        function, kind, file, anchor, nth, desc, &site, COMMIT, seal, commit,
    )
}

/// A ledger row audited at aterm `aterm` against the published seal at trust
/// `seal`, which reported it at `site`.
#[allow(clippy::too_many_arguments)]
fn row_at(
    function: &str,
    kind: &str,
    file: &str,
    anchor: &str,
    nth: &str,
    desc: &str,
    site: &str,
    aterm: &str,
    seal: &str,
    commit: &str,
) -> String {
    format!(
        "alpha\t{function}\t{kind}\t{file}\t{anchor}\t{nth}\t{desc}\t{site}\t{aterm}\t{seal}\t{commit}\t{REPRO}::guard_by_reaching_def\treviewer 2026-10-07 class A"
    )
}

/// The one obligation X the containment attacks' ledger rows name.
fn x(status: &str) -> Record {
    record(vec![ob(
        "alpha::f",
        ADD,
        "src/lib.rs",
        (6, 4, 6, 9),
        status,
        "a + 1",
        "arithmetic overflow (Add)",
    )])
}

fn x_row(seal: &str, commit: &str) -> String {
    row(
        "alpha::f",
        ADD,
        "src/lib.rs",
        "a + 1",
        "1/1",
        "arithmetic overflow (Add)",
        seal,
        commit,
    )
}

// === 1. Key collisions ======================================================

/// Two `vec![0u8; n]` in one function. Measured on the seal, the report locates both at the
/// standard library's macro body (`alloc/src/macros.rs:47:8-47:41`), one proved and one
/// failed, and the reduction leaves two rows identical but for the status: one exact key, one
/// ledger group. A candidate failing the proved one and proving the failed one must not have
/// the gain offset the loss.
#[test]
fn refute_out_of_tree_twins_swap_a_loss_for_a_gain() {
    let repo = checkout();
    let at = |st7: &str, st12: &str| {
        report(&[function_line(
            "alpha::zeros",
            &[
                (
                    "vc:alpha__zeros:custom:0",
                    "unbounded_allocation",
                    st7,
                    "alloc/src/macros.rs",
                    47,
                    8,
                    47,
                    41,
                    "unbounded allocation in `vec::from_elem` (count `_8`): bulk allocation may reach or exceed 268435456 elements",
                ),
                (
                    "vc:alpha__zeros:custom:1",
                    "unbounded_allocation",
                    st12,
                    "alloc/src/macros.rs",
                    47,
                    8,
                    47,
                    41,
                    "unbounded allocation in `vec::from_elem` (count `_10`): bulk allocation may reach or exceed 268435456 elements",
                ),
            ],
        )])
    };
    // The seal's statuses as measured; the candidate's swapped.
    let seal = reduced(&at("proved", "failed"), repo.path()).unwrap();
    let cand = reduced(&at("failed", "proved"), repo.path()).unwrap();
    // The reduction erased the one field that told them apart.
    assert_eq!(seal.rows[0].exact(), seal.rows[1].exact());
    let b = run_dir(SEAL, STUB, &seal);
    let c = run_dir(MAIN, STUB, &cand);
    let exit = gate(&b, &c, &Gate::default());
    assert_ne!(
        exit, 0,
        "ADMITTED: the seal proved `_8`'s bound (custom:0), the candidate does not, and nothing excuses it"
    );
}

/// The same within the tree: two obligations of one kind, span and description (the seal
/// reports a closure's `v[j] + v[j]` this way, `slice_bounds_check` twice per site), with the
/// seal proving the first and the candidate the second.
#[test]
fn refute_in_tree_same_site_twins_swap_a_loss_for_a_gain() {
    let repo = checkout();
    let at = |s0: &str, s1: &str| {
        report(&[function_line(
            "alpha::c::{closure#0}",
            &[
                (
                    "vc:alpha::c::{closure#0}:bounds:0",
                    "slice_bounds_check",
                    s0,
                    "src/lib.rs",
                    2,
                    23,
                    2,
                    27,
                    "slice bounds check",
                ),
                (
                    "vc:alpha::c::{closure#0}:bounds:2",
                    "slice_bounds_check",
                    s1,
                    "src/lib.rs",
                    2,
                    23,
                    2,
                    27,
                    "slice bounds check",
                ),
            ],
        )])
    };
    let seal = reduced(&at("proved", "failed"), repo.path()).unwrap();
    let cand = reduced(&at("failed", "proved"), repo.path()).unwrap();
    let b = run_dir(SEAL, STUB, &seal);
    let c = run_dir(MAIN, STUB, &cand);
    assert_ne!(
        gate(&b, &c, &Gate::default()),
        0,
        "ADMITTED: the seal proved bounds:0, the candidate does not"
    );
}

/// A retraction must not excuse a different obligation than its own. The seal proves two
/// `checked` overflow obligations an inlined `RawVec` growth puts in one function, at lines
/// 100 and 200 of `alloc/src/raw_vec.rs`. The ledger retracts X (nth 1, line 100); the
/// candidate still proves X (the row is stale) and loses Y, which no row names. Out of the
/// tree the exact key drops the position, so X's row must not pay for Y.
#[test]
fn refute_a_retraction_excuses_its_twin_and_hides_its_own_staleness() {
    let d = tmp("d27-refute-trust");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    let cc = commit(&t, "README", "main\n", "the candidate");
    let raw = "@sysroot/alloc/src/raw_vec.rs";
    let at = |x: &str, y: &str| {
        record(vec![
            ob(
                "alpha::grow",
                "arithmetic_overflow_mul",
                raw,
                (100, 8, 100, 30),
                x,
                "-",
                "arithmetic overflow (Mul)",
            ),
            ob(
                "alpha::grow",
                "arithmetic_overflow_mul",
                raw,
                (200, 8, 200, 30),
                y,
                "-",
                "arithmetic overflow (Mul)",
            ),
        ])
    };
    let seal = at("proved", "proved");
    let keys = trust_oblig::ledger_keys(&seal);
    assert_eq!((keys[0].nth, keys[1].nth), (1, 2));
    let r = row_at(
        "alpha::grow",
        "arithmetic_overflow_mul",
        raw,
        "-",
        "1/2",
        "arithmetic overflow (Mul)",
        &format!("{raw}:100:8-100:30"),
        COMMIT,
        &s,
        &f,
    );
    let b = run_dir(SEAL, &driver_of(&s), &seal);
    let c = run_dir(MAIN, &driver_of(&cc), &at("proved", "failed"));
    let exit = gate(
        &b,
        &c,
        &Gate {
            ledger_rows: vec![r],
            trust: Some(&t),
            base_commit: Some(&s),
            cand_commit: Some(&cc),
            ..Gate::default()
        },
    );
    assert_ne!(
        exit, 0,
        "ADMITTED: X (raw_vec.rs:100, the audited row) is still proved — the row is STALE — and Y (raw_vec.rs:200), which no row names, is lost"
    );
}

// === 2. Key drift ===========================================================

/// The ledger key's `nth` is a position among same-text twins. A row audited for X when X was
/// the function's only `a + 1` names nth 1; an unrelated edit then adds Y above X, and the row
/// names Y. The candidate loses Y (unaudited) and still proves X. The row claims the measured
/// tree as its audit's, as a re-stamp without a fresh audit would, and Y sits where X was:
/// only the group's size tells.
#[test]
fn refute_an_inserted_twin_inherits_the_audited_rows_excuse() {
    let d = tmp("d27-refute-trust");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    let cc = commit(&t, "README", "main\n", "the candidate");
    let add = |l: u32, st: &str| {
        ob(
            "alpha::step",
            ADD,
            "src/lib.rs",
            (l, 4, l, 9),
            st,
            "a + 1",
            "arithmetic overflow (Add)",
        )
    };
    // When the row was audited: X alone, at line 3.
    let then = trust_oblig::ledger_keys(&record(vec![add(3, "proved")]));
    let audited = &then[0];
    assert_eq!(audited.nth, 1);
    let r = format!(
        "alpha\t{}\tsrc/lib.rs:3:4-3:9\t{COMMIT}\t{s}\t{f}\t{REPRO}::guard_by_reaching_def\treviewer 2026-10-07 class A",
        audited.columns()
    );
    // Now: Y inserted at line 3, X pushed to line 7.
    let seal = record(vec![add(3, "proved"), add(7, "proved")]);
    let cand = record(vec![add(3, "failed"), add(7, "proved")]);
    let b = run_dir(SEAL, &driver_of(&s), &seal);
    let c = run_dir(MAIN, &driver_of(&cc), &cand);
    let exit = gate(
        &b,
        &c,
        &Gate {
            ledger_rows: vec![r],
            trust: Some(&t),
            base_commit: Some(&s),
            cand_commit: Some(&cc),
            ..Gate::default()
        },
    );
    assert_ne!(
        exit, 0,
        "ADMITTED: Y (line 3, never audited) is lost; X (line 7, the audited obligation) is still proved"
    );
}

// === 3. Duplicated rows =====================================================

/// A report naming one function twice is refused by the reduction.
#[test]
fn refute_the_reduction_accepts_a_function_reported_twice() {
    let repo = checkout();
    let twice = |a: &str, b: &str| {
        report(&[
            function_line(
                "alpha::f",
                &[(
                    "vc:alpha::f:add:0",
                    ADD,
                    a,
                    "src/lib.rs",
                    6,
                    4,
                    6,
                    9,
                    "arithmetic overflow (Add)",
                )],
            ),
            function_line(
                "alpha::f",
                &[(
                    "vc:alpha::f:add:0",
                    ADD,
                    b,
                    "src/lib.rs",
                    6,
                    4,
                    6,
                    9,
                    "arithmetic overflow (Add)",
                )],
            ),
        ])
    };
    let got = reduced(&twice("failed", "proved"), repo.path());
    assert!(
        got.is_err(),
        "a report naming alpha::f twice, with the SAME obligation id failed in one and proved in the other, was reduced to a record: {got:?}"
    );
}

/// A candidate whose own report says the seal's proved obligation failed, beside a duplicate
/// that proved, in one function record: its extra row at the seal's key is present on one side
/// only, which fails closed.
#[test]
fn refute_a_duplicate_proved_row_hides_the_candidates_failed_one() {
    let repo = checkout();
    let add = |id: &'static str, st: &'static str| -> O<'static> {
        (
            id,
            ADD,
            st,
            "src/lib.rs",
            6,
            4,
            6,
            9,
            "arithmetic overflow (Add)",
        )
    };
    let seal = reduced(
        &report(&[function_line(
            "alpha::f",
            &[add("vc:alpha::f:add:0", "proved")],
        )]),
        repo.path(),
    )
    .unwrap();
    let cand = reduced(
        &report(&[function_line(
            "alpha::f",
            &[
                add("vc:alpha::f:add:0", "failed"),
                add("vc:alpha::f:add:1", "proved"),
            ],
        )]),
        repo.path(),
    )
    .unwrap();
    let b = run_dir(SEAL, STUB, &seal);
    let c = run_dir(MAIN, STUB, &cand);
    assert_ne!(
        gate(&b, &c, &Gate::default()),
        0,
        "ADMITTED: the candidate reports vc:alpha::f:add:0 FAILED (and a duplicate proved)"
    );
}

// === 4. Commit containment: the wrong repository, a shallow one ============

/// An origin trust repository: main M0 -> F (the fix) -> M1 -> C (the candidate), and the seal
/// S on its own branch after F, so S contains F and F retracts nothing of the seal's. The rule:
/// a row whose commit the published seal already holds is COULD NOT COMPARE.
struct Origin {
    _d: aterm_tempfile::TempDir,
    root: PathBuf,
    f: String,
    c: String,
    s: String,
}

fn origin() -> Origin {
    let d = tmp("d27-refute-origin");
    let root = d.path().join("origin");
    init(&root);
    commit(&root, "README", "m0\n", "m0");
    let f = add_fix(&root);
    commit(&root, "README", "m1\n", "m1");
    let c = commit(&root, "README", "cand\n", "the candidate");
    git(&root, &["checkout", "-q", "-b", "seal", &f]);
    let s = commit(&root, "SEAL", "s\n", "the seal");
    git(&root, &["checkout", "-q", "main"]);
    Origin {
        _d: d,
        root,
        f,
        c,
        s,
    }
}

fn seal_holds_fix_gate(o: &Origin, trust: &Path) -> i32 {
    let b = run_dir(SEAL, &driver_of(&o.s), &x("proved"));
    let c = run_dir(MAIN, &driver_of(&o.c), &x("failed"));
    gate(
        &b,
        &c,
        &Gate {
            ledger_rows: vec![x_row(&o.s, &o.f)],
            trust: Some(trust),
            base_commit: Some(&o.s),
            cand_commit: Some(&o.c),
            ..Gate::default()
        },
    )
}

/// Control: in the origin itself the guard holds (exit 2).
#[test]
fn holds_a_row_already_in_the_seal_is_refused_in_a_full_repository() {
    let o = origin();
    assert_eq!(seal_holds_fix_gate(&o, &o.root), 2);
}

/// `--trust-repo` (default $HOME/trust) is a clone of main that never fetched the seal's branch:
/// the seal's commit is unknown there, which must not skip the "already in the seal" guard.
#[test]
fn refute_a_trust_repository_without_the_seal_commit_skips_the_seal_guard() {
    let o = origin();
    let d = tmp("d27-refute-clone");
    let t = d.path().join("trust");
    git(
        d.path(),
        &[
            "clone",
            "-q",
            "--single-branch",
            "--branch",
            "main",
            &format!("file://{}", o.root.display()),
            t.to_str().unwrap(),
        ],
    );
    assert_ne!(
        seal_holds_fix_gate(&o, &t),
        0,
        "ADMITTED: the seal holds F, so its proof of X is not one F retracts; the seal's commit was merely absent from --trust-repo"
    );
}

/// A shallow `--trust-repo`: main cloned at depth 3 (C, M1, F) and the seal's commit fetched at
/// depth 1. There `merge-base --is-ancestor F S` answers no (S's parents are cut) and
/// `--is-ancestor F C` yes, so the row would pass the seal guard and excuse the loss.
#[test]
fn refute_a_shallow_trust_repository_hides_a_fix_already_in_the_seal() {
    let o = origin();
    let d = tmp("d27-refute-shallow");
    let t = d.path().join("trust");
    git(
        d.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "3",
            "--single-branch",
            "--branch",
            "main",
            &format!("file://{}", o.root.display()),
            t.to_str().unwrap(),
        ],
    );
    git(&t, &["fetch", "-q", "--depth", "1", "origin", "seal"]);
    assert_eq!(git(&t, &["rev-parse", "--is-shallow-repository"]), "true");
    assert_ne!(
        seal_holds_fix_gate(&o, &t),
        0,
        "ADMITTED: a shallow --trust-repo answered `F is not in the seal` for a seal that holds F"
    );
}

/// `git replace` in `--trust-repo` (git honours replace refs in `merge-base --is-ancestor`)
/// must not make a candidate O lacking the fix read as containing it.
#[test]
fn refute_a_replace_ref_makes_a_candidate_contain_a_fix_it_lacks() {
    let d = tmp("d27-refute-replace");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    git(&t, &["checkout", "-q", "-b", "other", &s]);
    let o = commit(&t, "OTHER", "o\n", "a candidate without the fix");
    let b = run_dir(SEAL, &driver_of(&s), &x("proved"));
    let c = run_dir(MAIN, &driver_of(&o), &x("failed"));
    let g = Gate {
        ledger_rows: vec![x_row(&s, &f)],
        trust: Some(&t),
        base_commit: Some(&s),
        cand_commit: Some(&o),
        ..Gate::default()
    };
    // Control: O does not contain F, so the loss refuses.
    assert_eq!(gate(&b, &c, &g), 1);
    git(&t, &["replace", "--graft", &o, &f]);
    assert_ne!(
        gate(&b, &c, &g),
        0,
        "ADMITTED: candidate O lacks F; a replace ref in --trust-repo said it contains it"
    );
}

/// Ancestry alone ignores a revert: the candidate C sits after F and after `git revert F`, so
/// it does not hold the fix, and the row audited for F must not excuse its loss of X.
#[test]
fn refute_a_reverted_fix_still_excuses() {
    let d = tmp("d27-refute-revert");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    git(&t, &["revert", "--no-edit", &f]);
    let cc = commit(&t, "README", "main\n", "the candidate");
    let b = run_dir(SEAL, &driver_of(&s), &x("proved"));
    let c = run_dir(MAIN, &driver_of(&cc), &x("failed"));
    assert_ne!(
        gate(
            &b,
            &c,
            &Gate {
                ledger_rows: vec![x_row(&s, &f)],
                trust: Some(&t),
                base_commit: Some(&s),
                cand_commit: Some(&cc),
                ..Gate::default()
            },
        ),
        0,
        "ADMITTED: the candidate reverted F, so F retracts nothing of it"
    );
}

/// A graft file in `--trust-repo` (`info/grafts`, which git honours in `merge-base`) giving
/// candidate O the fix F as a parent: ancestry there is not what the commits record, so a
/// ledger with rows is not read there at all.
#[test]
fn refute_a_graft_file_makes_a_candidate_contain_a_fix_it_lacks() {
    let d = tmp("d27-refute-graft");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    git(&t, &["checkout", "-q", "-b", "other", &s]);
    let o = commit(&t, "OTHER", "o\n", "a candidate without the fix");
    let grafts = git(
        &t,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/grafts",
        ],
    );
    fs::create_dir_all(Path::new(&grafts).parent().unwrap()).unwrap();
    fs::write(&grafts, format!("{o} {f}\n")).unwrap();
    // The graft is live for a plain git: O "contains" F.
    let plain = crate::ledger::testing::git()
        .arg("-C")
        .arg(&t)
        .args(["merge-base", "--is-ancestor", &f, &o])
        .output()
        .unwrap();
    assert!(plain.status.success(), "the graft did not take");
    let b = run_dir(SEAL, &driver_of(&s), &x("proved"));
    let c = run_dir(MAIN, &driver_of(&o), &x("failed"));
    let exit = gate(
        &b,
        &c,
        &Gate {
            ledger_rows: vec![x_row(&s, &f)],
            trust: Some(&t),
            base_commit: Some(&s),
            cand_commit: Some(&o),
            ..Gate::default()
        },
    );
    assert_eq!(
        exit, 2,
        "candidate O lacks F; a graft file in --trust-repo said it contains it"
    );
}

/// A revert that says so (`This reverts commit F.`) withdraws the fix even when
/// it leaves the reproducer's file in place.
#[test]
fn refute_a_revert_that_keeps_the_reproducer_still_excuses() {
    let d = tmp("d27-refute-revert-msg");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    commit(
        &t,
        "vcgen.rs",
        "// the guard, undone\n",
        &format!("Revert \"fix: the false proof\"\n\nThis reverts commit {f}."),
    );
    let cc = commit(&t, "README", "main\n", "the candidate");
    let b = run_dir(SEAL, &driver_of(&s), &x("proved"));
    let c = run_dir(MAIN, &driver_of(&cc), &x("failed"));
    let g = Gate {
        ledger_rows: vec![x_row(&s, &f)],
        trust: Some(&t),
        base_commit: Some(&s),
        cand_commit: Some(&cc),
        ..Gate::default()
    };
    assert_eq!(gate(&b, &c, &g), 1, "ADMITTED: the candidate reverts F");
}

/// A fix undone by hand, its reproducer deleted with it: the candidate no longer holds what
/// pins the fix, and its loss refuses.
#[test]
fn refute_a_fix_whose_reproducer_the_candidate_dropped_still_excuses() {
    let d = tmp("d27-refute-dropped");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    git(&t, &["rm", "-q", REPRO]);
    git(&t, &["commit", "-q", "-m", "tidy tests"]);
    let cc = commit(&t, "README", "main\n", "the candidate");
    let b = run_dir(SEAL, &driver_of(&s), &x("proved"));
    let c = run_dir(MAIN, &driver_of(&cc), &x("failed"));
    let g = Gate {
        ledger_rows: vec![x_row(&s, &f)],
        trust: Some(&t),
        base_commit: Some(&s),
        cand_commit: Some(&cc),
        ..Gate::default()
    };
    assert_eq!(
        gate(&b, &c, &g),
        1,
        "ADMITTED: the candidate dropped F's reproducer"
    );
}

/// Control: through the same hardened git (no inherited `GIT_*`, no global or system config),
/// a candidate that holds the fix and does not prove the obligation it retracts is admitted,
/// the ledger's one purpose.
#[test]
fn holds_a_retraction_the_candidate_holds_still_excuses() {
    let d = tmp("d27-refute-control");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    let cc = commit(&t, "README", "main\n", "the candidate");
    let b = run_dir(SEAL, &driver_of(&s), &x("proved"));
    let c = run_dir(MAIN, &driver_of(&cc), &x("failed"));
    let g = Gate {
        ledger_rows: vec![x_row(&s, &f)],
        trust: Some(&t),
        base_commit: Some(&s),
        cand_commit: Some(&cc),
        ..Gate::default()
    };
    assert_eq!(gate(&b, &c, &g), 0);
    // …and of twins the candidate proves neither: the row's own is excused, the other lost.
    let raw = "@sysroot/alloc/src/raw_vec.rs";
    let at = |x: &str, y: &str| {
        record(vec![
            ob(
                "alpha::grow",
                "arithmetic_overflow_mul",
                raw,
                (100, 8, 100, 30),
                x,
                "-",
                "arithmetic overflow (Mul)",
            ),
            ob(
                "alpha::grow",
                "arithmetic_overflow_mul",
                raw,
                (100, 8, 100, 30),
                y,
                "-",
                "arithmetic overflow (Mul)",
            ),
        ])
    };
    let r = |nth: &str| {
        row_at(
            "alpha::grow",
            "arithmetic_overflow_mul",
            raw,
            "-",
            nth,
            "arithmetic overflow (Mul)",
            &format!("{raw}:100:8-100:30"),
            COMMIT,
            &s,
            &f,
        )
    };
    let b = run_dir(SEAL, &driver_of(&s), &at("proved", "proved"));
    let c = run_dir(MAIN, &driver_of(&cc), &at("failed", "failed"));
    let one = Gate {
        ledger_rows: vec![r("1/2")],
        ..g
    };
    assert_eq!(gate(&b, &c, &one), 1, "one twin is not retracted");
    let both = Gate {
        ledger_rows: vec![r("1/2"), r("2/2")],
        ..one
    };
    assert_eq!(gate(&b, &c, &both), 0, "both twins retracted");
}

// === 5. The count rule ======================================================

/// The seal's own check of alpha gave no result this run (`error[E0308]`, exit 101), and the
/// ratchet records the seal proving 5 of alpha's 10. A candidate proving 5 (rows 6-10, none of
/// rows 1-5) must not be admitted by the count without a per-obligation comparison.
#[test]
fn refute_a_seal_check_without_a_result_falls_back_to_the_count() {
    // A second library, beta, alike on both sides, so the runs compare.
    let with_beta = |prover: &str| {
        plan(prover, STUB).replace(
            "crate alpha\talpha@1.0.0\tgate\n",
            "crate alpha\talpha@1.0.0\tgate\ncrate beta\tbeta@1.0.0\tgate\n",
        )
    };
    let mut beta = x("proved");
    beta.package = "beta".into();
    let add_beta = |d: &Path| {
        fs::write(d.join("beta@1.0.0.log"), summary(1, 1)).unwrap();
        fs::write(d.join("beta@1.0.0.log.rc"), "0").unwrap();
        fs::write(d.join("beta@1.0.0.log.oblig.tsv"), beta.to_text()).unwrap();
    };
    let b = tmp("d27-refute-run");
    fs::write(b.path().join(".plan"), with_beta(SEAL)).unwrap();
    add_beta(b.path());
    fs::write(
        b.path().join("alpha@1.0.0.log"),
        "error[E0308]: mismatched types\n",
    )
    .unwrap();
    fs::write(b.path().join("alpha@1.0.0.log.rc"), "101").unwrap();
    let cand = record(
        (1..=10u32)
            .map(|i| {
                ob(
                    "alpha::f",
                    "assertion",
                    "src/lib.rs",
                    (i, 0, i, 1),
                    if i > 5 { "proved" } else { "failed" },
                    &format!("row {i}"),
                    "assertion",
                )
            })
            .collect(),
    );
    let c = run_dir(MAIN, STUB, &cand);
    fs::write(c.path().join(".plan"), with_beta(MAIN)).unwrap();
    add_beta(c.path());
    let exit = gate(
        &b,
        &c,
        &Gate {
            ratchet: Some(format!("alpha\t5\t10\t{STUB}\n")),
            ..Gate::default()
        },
    );
    assert_ne!(
        exit, 0,
        "ADMITTED by count: nothing compared alpha obligation by obligation"
    );
}

// === 6. Attacks the gate withstands =========================================

/// Wildcards, prefixes and un-normalised fields in a ledger row: none excuses.
#[test]
fn holds_no_wildcard_or_prefix_row_excuses() {
    let d = tmp("d27-refute-trust");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    let cc = commit(&t, "README", "main\n", "the candidate");
    let b = run_dir(SEAL, &driver_of(&s), &x("proved"));
    let c = run_dir(MAIN, &driver_of(&cc), &x("failed"));
    let d_ = "arithmetic overflow (Add)";
    for r in [
        row("alpha::*", ADD, "src/lib.rs", "a + 1", "1/1", d_, &s, &f),
        row("alpha::", ADD, "src/lib.rs", "a + 1", "1/1", d_, &s, &f),
        row("*", ADD, "src/lib.rs", "a + 1", "1/1", d_, &s, &f),
        row("alpha::f", "*", "src/lib.rs", "a + 1", "1/1", d_, &s, &f),
        row("alpha::f", ADD, "src/*", "a + 1", "1/1", d_, &s, &f),
        row("alpha::f", ADD, "src/lib.rs", "*", "1/1", d_, &s, &f),
        row("alpha::f", ADD, "src/lib.rs", "a +", "1/1", d_, &s, &f),
        row("alpha::f", ADD, "src/lib.rs", "a + 1", "*", d_, &s, &f),
        row("alpha::f", ADD, "src/lib.rs", "a + 1", "1/*", d_, &s, &f),
        // The v1 form (a bare place), and a place or count out of form.
        row("alpha::f", ADD, "src/lib.rs", "a + 1", "1", d_, &s, &f),
        row("alpha::f", ADD, "src/lib.rs", "a + 1", "0/1", d_, &s, &f),
        row("alpha::f", ADD, "src/lib.rs", "a + 1", "01/1", d_, &s, &f),
        // A count the seal's group is not: it names nothing for certain.
        row("alpha::f", ADD, "src/lib.rs", "a + 1", "1/2", d_, &s, &f),
        row("alpha::f", ADD, "src/lib.rs", "a + 1", "1/1", "*", &s, &f),
        row(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            "arithmetic overflow",
            &s,
            &f,
        ),
        row(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            &s,
            &f[..12],
        ),
        row(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            &s,
            &f.to_uppercase(),
        ),
        row("alpha::f", ADD, "src/lib.rs", "a  + 1", "1/1", d_, &s, &f),
        row("alpha::f", ADD, "@none", "a + 1", "1/1", d_, &s, &f),
        // The site and aterm commit (v3): a wildcard, no span, another span, another file's;
        // a commit short, upper-case, a wildcard, or one the runs did not measure (with no
        // repository holding the ledger to compare trees in).
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "*",
            COMMIT,
            &s,
            &f,
        ),
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/lib.rs",
            COMMIT,
            &s,
            &f,
        ),
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/lib.rs:6:4-6:10",
            COMMIT,
            &s,
            &f,
        ),
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/main.rs:6:4-6:9",
            COMMIT,
            &s,
            &f,
        ),
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/lib.rs:6:4-6:9",
            &COMMIT[..12],
            &s,
            &f,
        ),
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/lib.rs:6:4-6:9",
            &COMMIT.to_uppercase(),
            &s,
            &f,
        ),
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/lib.rs:6:4-6:9",
            "*",
            &s,
            &f,
        ),
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/lib.rs:6:4-6:9",
            TREE,
            &s,
            &f,
        ),
        // The seal it was audited against: a wildcard, short, or another
        // commit than the base ran (the fix's, here).
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/lib.rs:6:4-6:9",
            COMMIT,
            "*",
            &f,
        ),
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/lib.rs:6:4-6:9",
            COMMIT,
            &s[..12],
            &f,
        ),
        row_at(
            "alpha::f",
            ADD,
            "src/lib.rs",
            "a + 1",
            "1/1",
            d_,
            "src/lib.rs:6:4-6:9",
            COMMIT,
            &f,
            &f,
        ),
        // The v2 form: ten fields, no site and no aterm commit.
        format!(
            "alpha\talpha::f\t{ADD}\tsrc/lib.rs\ta + 1\t1/1\t{d_}\t{f}\t{REPRO}::guard_by_reaching_def\treviewer 2026-10-07 class A"
        ),
    ] {
        let exit = gate(
            &b,
            &c,
            &Gate {
                ledger_rows: vec![r.clone()],
                trust: Some(&t),
                base_commit: Some(&s),
                cand_commit: Some(&cc),
                ..Gate::default()
            },
        );
        assert_ne!(exit, 0, "{r}");
    }
    // Two identical rows: the duplicate is an error, never two units.
    let exit = gate(
        &b,
        &c,
        &Gate {
            ledger_rows: vec![x_row(&s, &f), x_row(&s, &f)],
            trust: Some(&t),
            base_commit: Some(&s),
            cand_commit: Some(&cc),
            ..Gate::default()
        },
    );
    assert_eq!(exit, 2);
}

/// The candidate's commit: absent, short, upper-case, or another toolchain's.
#[test]
fn holds_a_candidate_commit_that_is_missing_or_not_its_runs_applies_nothing() {
    let d = tmp("d27-refute-trust");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    let cc = commit(&t, "README", "main\n", "the candidate");
    let b = run_dir(SEAL, &driver_of(&s), &x("proved"));
    let c = run_dir(MAIN, &driver_of(&cc), &x("failed"));
    let upper = cc.to_uppercase();
    for (given, want) in [
        (None, 1),
        (Some(&cc[..9]), 1),
        (Some(upper.as_str()), 1),
        (Some(s.as_str()), 2),
        (Some(f.as_str()), 2),
    ] {
        let exit = gate(
            &b,
            &c,
            &Gate {
                ledger_rows: vec![x_row(&s, &f)],
                trust: Some(&t),
                base_commit: Some(&s),
                cand_commit: given,
                ..Gate::default()
            },
        );
        assert_eq!(exit, want, "{given:?}");
    }
    // A driver line that names no commit: nothing ties the given one to the run.
    let c = run_dir(MAIN, STUB, &x("failed"));
    let exit = gate(
        &b,
        &c,
        &Gate {
            ledger_rows: vec![x_row(&s, &f)],
            trust: Some(&t),
            base_commit: Some(&s),
            cand_commit: Some(&cc),
            ..Gate::default()
        },
    );
    assert_ne!(exit, 0);
}

/// Truncation: a record cut short, a report cut before its footer, a log cut
/// before its Coverage line.
#[test]
fn holds_a_truncated_record_report_or_log_is_never_a_pass() {
    let repo = checkout();
    let rows = |st: &str| {
        (1..=4u32)
            .map(|i| {
                ob(
                    "alpha::f",
                    "assertion",
                    "src/lib.rs",
                    (i, 0, i, 1),
                    st,
                    &format!("row {i}"),
                    "assertion",
                )
            })
            .collect::<Vec<_>>()
    };
    let seal = record(rows("proved"));
    let b = run_dir(SEAL, STUB, &seal);
    // The candidate's record cut at a line boundary (its `end` gone), counts kept.
    let c = run_dir(MAIN, STUB, &seal);
    let text = seal.to_text();
    let cut: String = text.lines().take(7).map(|l| format!("{l}\n")).collect();
    fs::write(c.path().join("alpha@1.0.0.log.oblig.tsv"), cut).unwrap();
    assert_ne!(gate(&b, &c, &Gate::default()), 0);
    // A record rewritten to the rows that survived: the log's Summary still says 4.
    let mut short = seal.clone();
    short.rows.truncate(2);
    fs::write(c.path().join("alpha@1.0.0.log.oblig.tsv"), short.to_text()).unwrap();
    assert_ne!(gate(&b, &c, &Gate::default()), 0);
    // A log cut before its Coverage line.
    let c = run_dir(MAIN, STUB, &seal);
    fs::write(
        c.path().join("alpha@1.0.0.log"),
        "Summary: 4 proved, 0 failed (4 total)\n",
    )
    .unwrap();
    assert_ne!(gate(&b, &c, &Gate::default()), 0);
    // A report cut before its footer, at a line boundary.
    let full = report(&[function_line(
        "alpha::f",
        &[(
            "vc:alpha::f:add:0",
            ADD,
            "proved",
            "src/lib.rs",
            6,
            4,
            6,
            9,
            "arithmetic overflow (Add)",
        )],
    )]);
    let cut: String = full.lines().take(2).map(|l| format!("{l}\n")).collect();
    assert!(reduced(&cut, repo.path()).is_err());
}

// === 7. The tree a row was audited in =======================================

/// A trust repository: the seal S, the fix F (its reproducer), the candidate C.
struct TrustFix {
    _d: aterm_tempfile::TempDir,
    t: PathBuf,
    s: String,
    f: String,
    cc: String,
}

fn trust_fix() -> TrustFix {
    let d = tmp("d27-refute-trust");
    let t = d.path().join("trust");
    init(&t);
    let s = commit(&t, "README", "seal\n", "the seal");
    let f = add_fix(&t);
    let cc = commit(&t, "README", "main\n", "the candidate");
    TrustFix { _d: d, t, s, f, cc }
}

/// An aterm repository holding the ledger where the gate reads it: commit `audited` (the files
/// `then`, an empty ledger), then commit `measured` (the files `now`, the ledger holding
/// `rows(audited)`), the commit both runs measure.
struct Aterm {
    _d: aterm_tempfile::TempDir,
    ledger: PathBuf,
    audited: String,
    measured: String,
}

fn aterm(then: &[(&str, &str)], now: &[(&str, &str)], rows: impl Fn(&str) -> Vec<String>) -> Aterm {
    let d = tmp("d27-refute-aterm");
    let root = d.path().join("aterm");
    init(&root);
    let write = |files: &[(&str, &str)]| {
        for (p, text) in files {
            let p = root.join(p);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
    };
    write(then);
    let audited = commit(
        &root,
        trust_oblig::LEDGER_PATH,
        &format!("{LEDGER_MAGIC}\n"),
        "where the row was audited",
    );
    write(now);
    let mut text = format!("{LEDGER_MAGIC}\n");
    for r in rows(&audited) {
        text.push_str(&r);
        text.push('\n');
    }
    let measured = commit(
        &root,
        trust_oblig::LEDGER_PATH,
        &text,
        "the row, and what else moved",
    );
    Aterm {
        _d: d,
        ledger: root.join(trust_oblig::LEDGER_PATH),
        audited,
        measured,
    }
}

/// The gate over the seal's and the candidate's records of `a.measured`, with the ledger
/// committed there and the candidate holding the fix.
fn audited_gate(a: &Aterm, tr: &TrustFix, seal: &Record, cand: &Record) -> i32 {
    let b = run_dir_at(SEAL, &driver_of(&tr.s), seal, &a.measured);
    let c = run_dir_at(MAIN, &driver_of(&tr.cc), cand, &a.measured);
    gate(
        &b,
        &c,
        &Gate {
            ledger_at: Some(&a.ledger),
            trust: Some(&tr.t),
            base_commit: Some(&tr.s),
            cand_commit: Some(&tr.cc),
            ..Gate::default()
        },
    )
}

/// `a + 1` of `alpha::step` at line `l` (columns 4-9), its status `st`.
fn step_add(l: u32, st: &str) -> Obligation {
    ob(
        "alpha::step",
        ADD,
        "src/lib.rs",
        (l, 4, l, 9),
        st,
        "a + 1",
        "arithmetic overflow (Add)",
    )
}

/// A row audited at aterm `aterm` for the `a + 1` the seal reported at line `l`, the `nth` of
/// such in `alpha::step`.
fn step_row(nth: &str, l: u32, aterm: &str, seal: &str, fix: &str) -> String {
    row_at(
        "alpha::step",
        ADD,
        "src/lib.rs",
        "a + 1",
        nth,
        "arithmetic overflow (Add)",
        &format!("src/lib.rs:{l}:4-{l}:9"),
        aterm,
        seal,
        fix,
    )
}

/// `alpha::step` as audited: X, the `a + 1` the row was audited for, at line 3,
/// and Z, another, at line 7.
const AUDITED: &str = concat!(
    "pub fn step(a: u32, b: bool) -> u32 {\n", // 1
    "    let x = b;\n",                        // 2
    "    a + 1; // X: the one audited\n",      // 3
    "    if x {\n",                            // 4
    "        return 0;\n",                     // 5
    "    }\n",                                 // 6
    "    a + 1 // Z\n",                        // 7
    "}\n",
);

/// Controls in the tree the row was audited in (the row committed on top, nothing else
/// changed): a candidate losing X is excused; one still proving X while losing Z makes the row
/// stale (exit 2).
#[test]
fn holds_a_row_in_the_tree_it_was_audited_in_excuses_only_its_own() {
    let tr = trust_fix();
    let a = aterm(&[("src/lib.rs", AUDITED)], &[], |at| {
        vec![step_row("1/2", 3, at, &tr.s, &tr.f)]
    });
    // Audited at another commit than the runs measure: their trees compared.
    assert_ne!(a.audited, a.measured);
    let seal = record(vec![step_add(3, "proved"), step_add(7, "proved")]);
    let lose_x = record(vec![step_add(3, "failed"), step_add(7, "proved")]);
    assert_eq!(audited_gate(&a, &tr, &seal, &lose_x), 0);
    let lose_z = record(vec![step_add(3, "proved"), step_add(7, "failed")]);
    assert_eq!(audited_gate(&a, &tr, &seal, &lose_z), 2);
}

/// X deleted and W, another `a + 1`, added at line 2, so the group is still two (W 1/2,
/// Z 2/2): the candidate loses W, which nobody audited.
#[test]
fn refute_a_look_alike_replaced_at_the_same_group_size_inherits_the_audited_rows_excuse() {
    let tr = trust_fix();
    let replaced = concat!(
        "pub fn step(a: u32, b: bool) -> u32 {\n",  // 1
        "    a + 1; // W: added since the audit\n", // 2
        "    let x = b;\n",                         // 3
        "    if x {\n",                             // 4
        "        return 0;\n",                      // 5
        "    }\n",                                  // 6
        "    a + 1 // Z\n",                         // 7
        "}\n",
    );
    let a = aterm(
        &[("src/lib.rs", AUDITED)],
        &[("src/lib.rs", replaced)],
        |at| vec![step_row("1/2", 3, at, &tr.s, &tr.f)],
    );
    let seal = record(vec![step_add(2, "proved"), step_add(7, "proved")]);
    let lose_w = record(vec![step_add(2, "failed"), step_add(7, "proved")]);
    assert_eq!(
        audited_gate(&a, &tr, &seal, &lose_w),
        1,
        "ADMITTED: the candidate loses W (line 2), added after the audit of X, which no longer exists"
    );
    // W replaces X at X's own place, `a` now another value there: rank, size and site agree.
    let in_place = AUDITED
        .replace(
            "    let x = b;\n",
            "    let (x, a) = (b, a.wrapping_mul(3));\n",
        )
        .replace("a + 1; // X: the one audited", "a + 1; // W: X's place");
    let a = aterm(
        &[("src/lib.rs", AUDITED)],
        &[("src/lib.rs", &in_place)],
        |at| vec![step_row("1/2", 3, at, &tr.s, &tr.f)],
    );
    let lose_w = record(vec![step_add(3, "failed"), step_add(7, "proved")]);
    assert_eq!(
        audited_gate(&a, &tr, &seal_at_3_and_7(), &lose_w),
        1,
        "ADMITTED: the candidate loses W, which replaced the audited X at its place"
    );
}

fn seal_at_3_and_7() -> Record {
    record(vec![step_add(3, "proved"), step_add(7, "proved")])
}

/// Z moved above X (Z at line 3, X at 7): the row's rank and site now name Z, which the
/// candidate loses while it still proves X.
#[test]
fn refute_two_look_alikes_reordered_hand_the_row_to_the_other() {
    let tr = trust_fix();
    let reordered = concat!(
        "pub fn step(a: u32, b: bool) -> u32 {\n", // 1
        "    let x = b;\n",                        // 2
        "    a + 1; // Z\n",                       // 3
        "    if x {\n",                            // 4
        "        return 0;\n",                     // 5
        "    }\n",                                 // 6
        "    a + 1 // X: the one audited\n",       // 7
        "}\n",
    );
    let a = aterm(
        &[("src/lib.rs", AUDITED)],
        &[("src/lib.rs", reordered)],
        |at| vec![step_row("1/2", 3, at, &tr.s, &tr.f)],
    );
    let lose_z = record(vec![step_add(3, "failed"), step_add(7, "proved")]);
    assert_eq!(
        audited_gate(&a, &tr, &seal_at_3_and_7(), &lose_z),
        1,
        "ADMITTED: the candidate loses Z, moved to where X was; X, the audited one, it still proves"
    );
}

/// The `1/1` form: the function's only `a + 1` deleted and another added in its place.
#[test]
fn refute_a_lone_look_alike_deleted_and_added_again_inherits_the_audited_rows_excuse() {
    let tr = trust_fix();
    let then = "pub fn step(a: u32) -> u32 {\n    let x = a;\n    a + 1 // X\n}\n";
    let now = "pub fn step(a: u32) -> u32 {\n    let a = a / 2;\n    a + 1 // W\n}\n";
    let a = aterm(&[("src/lib.rs", then)], &[("src/lib.rs", now)], |at| {
        vec![step_row("1/1", 3, at, &tr.s, &tr.f)]
    });
    assert_eq!(
        audited_gate(
            &a,
            &tr,
            &record(vec![step_add(3, "proved")]),
            &record(vec![step_add(3, "failed")])
        ),
        1,
        "ADMITTED: the candidate loses W, which replaced the audited X"
    );
}

/// Why the whole tree and not the obligation's file: an obligation located in a macro body
/// (`src/mac.rs`) or the standard library moves with an edit to its function's own file, which
/// its file never sees. Here `alpha::step`'s one `inc!` now takes another value: file, rank,
/// size and site agree, and the obligation is not the one audited.
#[test]
fn refute_an_edit_to_the_function_moves_an_obligation_located_elsewhere() {
    let tr = trust_fix();
    let mac = "macro_rules! inc {\n    ($v:expr) => { $v + 1 };\n}\n";
    let then = "pub fn step(x: u32, w: u32) -> u32 {\n    inc!(x)\n}\n";
    let now = "pub fn step(x: u32, w: u32) -> u32 {\n    inc!(w)\n}\n";
    let in_mac = |st: &str| {
        record(vec![ob(
            "alpha::step",
            ADD,
            "src/mac.rs",
            (2, 19, 2, 25),
            st,
            "$v + 1",
            "arithmetic overflow (Add)",
        )])
    };
    let a = aterm(
        &[("src/mac.rs", mac), ("src/lib.rs", then)],
        &[("src/lib.rs", now)],
        |at| {
            vec![row_at(
                "alpha::step",
                ADD,
                "src/mac.rs",
                "$v + 1",
                "1/1",
                "arithmetic overflow (Add)",
                "src/mac.rs:2:19-2:25",
                at,
                &tr.s,
                &tr.f,
            )]
        },
    );
    assert_eq!(
        audited_gate(&a, &tr, &in_mac("proved"), &in_mac("failed")),
        1,
        "ADMITTED: src/mac.rs is unchanged, and its obligation is now `w + 1`'s"
    );
    // The standard library's: `alpha::grow` grows another vector now.
    let raw = "@sysroot/alloc/src/raw_vec.rs";
    let in_std = |st: &str| {
        record(vec![ob(
            "alpha::grow",
            "arithmetic_overflow_mul",
            raw,
            (100, 8, 100, 30),
            st,
            "-",
            "arithmetic overflow (Mul)",
        )])
    };
    let a = aterm(
        &[(
            "src/lib.rs",
            "pub fn grow(v: &mut Vec<u8>, w: &mut Vec<u8>) {\n    v.push(1);\n}\n",
        )],
        &[(
            "src/lib.rs",
            "pub fn grow(v: &mut Vec<u8>, w: &mut Vec<u8>) {\n    w.push(1);\n}\n",
        )],
        |at| {
            vec![row_at(
                "alpha::grow",
                "arithmetic_overflow_mul",
                raw,
                "-",
                "1/1",
                "arithmetic overflow (Mul)",
                &format!("{raw}:100:8-100:30"),
                at,
                &tr.s,
                &tr.f,
            )]
        },
    );
    assert_eq!(
        audited_gate(&a, &tr, &in_std("proved"), &in_std("failed")),
        1,
        "ADMITTED: the obligation in raw_vec.rs is now another vector's growth"
    );
}

/// A v2 ledger, committed where the gate reads it: not read at all.
#[test]
fn holds_a_v2_ledger_is_refused_whole() {
    let tr = trust_fix();
    let a = aterm(&[("src/lib.rs", AUDITED)], &[], |at| {
        vec![step_row("1/2", 3, at, &tr.s, &tr.f)]
    });
    fs::write(
        &a.ledger,
        format!(
            "# aterm trust-gate retractions v2\nalpha\talpha::step\t{ADD}\tsrc/lib.rs\ta + 1\t1/2\tarithmetic overflow (Add)\t{}\t{REPRO}::guard_by_reaching_def\treviewer 2026-10-07 class A\n",
            tr.f
        ),
    )
    .unwrap();
    let lose_x = record(vec![step_add(3, "failed"), step_add(7, "proved")]);
    assert_eq!(audited_gate(&a, &tr, &seal_at_3_and_7(), &lose_x), 2);
}

/// Another seal, the same tree: two bounds checks at one site of `alpha::sum`, told apart only
/// by their MIR locals. The row was audited for `_8`'s, the first seal S1 reported; the
/// published seal S2 reports `_10`'s first, so the row's rank, size and site name `_10`'s,
/// which the candidate loses while it proves `_8`'s.
#[test]
fn refute_another_seal_reorders_look_alikes_at_one_site() {
    let d = tmp("d27-refute-trust");
    let t = d.path().join("trust");
    init(&t);
    let s1 = commit(
        &t,
        "README",
        "seal 1\n",
        "the seal the row was audited against",
    );
    let s2 = commit(&t, "README", "seal 2\n", "the seal published since");
    let f = add_fix(&t);
    let cc = commit(&t, "README", "main\n", "the candidate");
    let check = |local: &str, st: &str| {
        ob(
            "alpha::sum",
            "slice_bounds_check",
            "src/lib.rs",
            (2, 23, 2, 27),
            st,
            "v[j]",
            &format!("slice bounds check (index `{local}`)"),
        )
    };
    // S2 reports `_10`'s first; the candidate loses it and proves `_8`'s.
    let seal = record(vec![check("_10", "proved"), check("_8", "proved")]);
    let cand = record(vec![check("_10", "failed"), check("_8", "proved")]);
    let row = |audited_against: &str| {
        row_at(
            "alpha::sum",
            "slice_bounds_check",
            "src/lib.rs",
            "v[j]",
            "1/2",
            "slice bounds check (index `_`)",
            "src/lib.rs:2:23-2:27",
            COMMIT,
            audited_against,
            &f,
        )
    };
    let b = run_dir(SEAL, &driver_of(&s2), &seal);
    let c = run_dir(MAIN, &driver_of(&cc), &cand);
    let gate_with = |r: String| {
        gate(
            &b,
            &c,
            &Gate {
                ledger_rows: vec![r],
                trust: Some(&t),
                base_commit: Some(&s2),
                cand_commit: Some(&cc),
                ..Gate::default()
            },
        )
    };
    assert_eq!(
        gate_with(row(&s1)),
        1,
        "ADMITTED: the row audited `_8`'s against S1 names `_10`'s under S2"
    );
    // Control: re-audited against S2 (for `_10`'s, the first there), it applies.
    assert_eq!(gate_with(row(&s2)), 0);
}
