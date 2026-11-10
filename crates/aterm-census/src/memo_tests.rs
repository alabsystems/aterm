// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The census memo's tests: what a run reuses, what it judges again, and what it keeps. Each
//! question kind has a change that keeps its answer (the unit is reused) and one that changes it
//! (the unit is judged again), so a memo that reused too much or too little fails one of them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use super::*;
use crate::guards::{Corpus, read_corpus};
use crate::{CENSUSES, Census, CensusOutcome, Walk, run_censuses};

const A: &str = "crates/x/src/a.rs";
const B: &str = "crates/x/src/b.rs";
const DOC: &str = "docs/x.md";
const NO_ENGINE: &crate::EngineBaseline = &Err(String::new());

fn checkout(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(rel, text)| ((*rel).to_string(), (*text).to_string()))
        .collect();
    crate::scan_set::test_fixtures::disk_tree("aterm-census-memo", name, &files)
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

/// The memo of the checkout at `root`, kept where no walk reads (`target/`), keyed by `code`.
fn memo_at(root: &Path, code: u8) -> Memo {
    Memo::with_code(root.join("target/census.memo"), [code; 32])
}

/// A census whose one unit, `id`, reads `rel` and reports its length.
fn reading(walk: &Walk<'_>, id: &str, rel: &str) -> CensusOutcome {
    walk.whole(id, || {
        let text = crate::tree::read_to_string(&walk.root.join(rel)).unwrap_or_default();
        CensusOutcome::green(
            "reads",
            String::new(),
            format!("{rel}: {} byte(s)", text.len()),
        )
    })
}

fn reads_a(walk: &Walk<'_>) -> CensusOutcome {
    reading(walk, "census:a", A)
}

fn reads_b(walk: &Walk<'_>) -> CensusOutcome {
    reading(walk, "census:b", B)
}

fn reads_doc(walk: &Walk<'_>) -> CensusOutcome {
    reading(walk, "census:doc", DOC)
}

const READERS: &[Census] = &[
    Census {
        name: "a",
        about: "",
        run: reads_a,
    },
    Census {
        name: "b",
        about: "",
        run: reads_b,
    },
    Census {
        name: "doc",
        about: "",
        run: reads_doc,
    },
];

/// `censuses` over `root` with `memo`: each verdict, and what the memo reused.
fn run_with(root: &Path, censuses: &[Census], memo: &Memo) -> (Vec<String>, MemoReport) {
    let selected: Vec<&Census> = censuses.iter().collect();
    let (outcomes, report) = run_censuses(root, &selected, NO_ENGINE, Some(memo));
    (
        outcomes.into_iter().map(|o| o.verdict).collect(),
        report.expect("a memo run reports"),
    )
}

fn judged_again(report: &MemoReport) -> Vec<&str> {
    report.rejudged.iter().map(|(id, _)| id.as_str()).collect()
}

fn files() -> [(&'static str, &'static str); 3] {
    [(A, "fn a() {}\n"), (B, "fn b() {}\n"), (DOC, "notes\n")]
}

/// An unchanged tree reuses every unit's result, and the results are the ones the units gave.
/// NEGATIVE CONTROL: the first run, with no record, reuses none.
#[test]
fn an_unchanged_tree_reuses_every_unit() {
    let root = checkout("unchanged", &files());
    let memo = memo_at(&root, 1);
    let (cold, first) = run_with(&root, READERS, &memo);
    let (warm, second) = run_with(&root, READERS, &memo);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(first.no_record.as_deref(), Some("none kept yet"), "{first}");
    assert!(first.reused.is_empty(), "{first}");
    assert_eq!(
        second.reused,
        ["census:a", "census:b", "census:doc"],
        "{second}"
    );
    assert!(
        second.rejudged.is_empty() && second.no_record.is_none() && second.not_kept.is_none(),
        "{second}"
    );
    assert_eq!(cold, warm);
}

/// A changed file judges again exactly the units that read it, a source file (a snapshot
/// question) and any other file (a tree question) alike, and their results are the new ones.
/// NEGATIVE CONTROL: the unit that read the other file is reused.
#[test]
fn a_changed_file_judges_again_only_the_units_that_read_it() {
    let root = checkout("changed", &files());
    let memo = memo_at(&root, 1);
    let _ = run_with(&root, READERS, &memo);
    write(&root, A, "fn a() { let _ = 1; }\n");
    write(&root, DOC, "more notes\n");
    let (verdicts, report) = run_with(&root, READERS, &memo);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(report.reused, ["census:b"], "{report}");
    assert_eq!(
        judged_again(&report),
        ["census:a", "census:doc"],
        "{report}"
    );
    assert!(
        report.line().contains("crates/x/src/a.rs changed"),
        "it names what changed: {report}"
    );
    assert!(
        verdicts[0].ends_with("crates/x/src/a.rs: 22 byte(s)"),
        "{verdicts:?}"
    );
    assert!(
        verdicts[2].ends_with("docs/x.md: 11 byte(s)"),
        "{verdicts:?}"
    );
}

/// Another census version is no record: the run judges everything again, and keeps its own.
/// NEGATIVE CONTROL: the same version, run again, reuses everything.
#[test]
fn a_census_version_bump_judges_everything_again() {
    let root = checkout("version", &files());
    let _ = run_with(&root, READERS, &memo_at(&root, 1));
    let (_, bumped) = run_with(&root, READERS, &memo_at(&root, 2));
    let (_, again) = run_with(&root, READERS, &memo_at(&root, 2));
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        bumped.no_record.as_deref(),
        Some("it was made by other census code"),
        "{bumped}"
    );
    assert!(bumped.reused.is_empty(), "{bumped}");
    assert_eq!(again.reused.len(), READERS.len(), "{again}");
}

/// The guards' whole-graph rule: a caller that did not change is judged again when the item it
/// calls is newly gated in another file, and its finding is the one a run with no memo finds.
/// NEGATIVE CONTROL: before the gate, no run finds it; and a guard that read neither file is
/// reused.
#[test]
fn a_newly_gated_item_is_found_at_its_unchanged_caller() {
    const LIB: &str = "crates/aterm-pty/src/lib.rs";
    const CALLER: &str = "crates/aterm-pty/src/other.rs";
    const ITEM: &str = "crates/aterm-pty/src/unix.rs";
    let root = checkout(
        "seam",
        &[
            (LIB, "pub mod unix;\nmod other;\n"),
            (CALLER, "fn g() { super::unix::ensure_above_stdio(); }\n"),
            (ITEM, "pub fn ensure_above_stdio() {}\n"),
        ],
    );
    let guards: Vec<&Census> = CENSUSES.iter().filter(|c| c.name == "guards").collect();
    let seam_lines = |memo: Option<&Memo>| -> (Vec<String>, String, Option<MemoReport>) {
        let (outcomes, report) = run_censuses(&root, &guards, NO_ENGINE, memo);
        let log = outcomes.into_iter().map(|o| o.log).collect::<String>();
        let lines = log
            .lines()
            .filter(|l| l.contains("[seam-caller]"))
            .map(str::to_string)
            .collect();
        (lines, log, report)
    };
    let memo = memo_at(&root, 1);
    let (before, _, _) = seam_lines(Some(&memo));
    write(
        &root,
        ITEM,
        "#[cfg(any(test, debug_assertions))]\npub fn ensure_above_stdio() {}\n",
    );
    let (after, with_memo, report) = seam_lines(Some(&memo));
    let (fresh, without_memo, _) = seam_lines(None);
    let _ = std::fs::remove_dir_all(&root);
    let report = report.expect("a memo run reports");
    assert!(before.is_empty(), "nothing is gated yet: {before:?}");
    assert_eq!(after.len(), 1, "{after:?}\n{report}");
    assert!(
        after[0].contains("crates/aterm-pty/src/other.rs:1"),
        "{after:?}"
    );
    assert_eq!(after, fresh);
    assert_eq!(
        with_memo, without_memo,
        "the memo run's transcript is a fresh run's"
    );
    assert!(
        report
            .rejudged
            .iter()
            .any(|(id, why)| id == "guard:seam-caller"
                && matches!(why, Rejudged::Changed(what) if what.starts_with(ITEM))),
        "{report}"
    );
    assert!(
        report.reused.iter().any(|id| id == "guard:rustup-keg"),
        "a rule that read neither file is reused: {report}"
    );
}

/// A run in which a unit reads what the memo cannot follow keeps nothing.
/// NEGATIVE CONTROL: the readers alone keep a record, which the next run reuses.
#[test]
fn an_untraceable_read_keeps_no_record() {
    fn opens(walk: &Walk<'_>) -> CensusOutcome {
        walk.whole("census:open", || {
            let _ = crate::tree::open_regular(&walk.root.join(A));
            CensusOutcome::green("open", String::new(), "opened")
        })
    }
    let root = checkout("untraceable", &files());
    let opening = [Census {
        name: "open",
        about: "",
        run: opens,
    }];
    let (_, first) = run_with(&root, &opening, &memo_at(&root, 1));
    let (_, second) = run_with(&root, &opening, &memo_at(&root, 1));
    let (_, kept) = run_with(&root, READERS, &memo_at(&root, 3));
    let (_, reused) = run_with(&root, READERS, &memo_at(&root, 3));
    let _ = std::fs::remove_dir_all(&root);
    assert!(first.not_kept.is_some(), "{first}");
    assert_eq!(
        second.no_record.as_deref(),
        Some("none kept yet"),
        "{second}"
    );
    assert!(kept.not_kept.is_none(), "{kept}");
    assert_eq!(reused.reused.len(), READERS.len(), "{reused}");
}

/// A record cut short or changed in one byte is no record.
/// NEGATIVE CONTROL: the record as written is one.
#[test]
fn a_damaged_record_is_no_record() {
    let root = checkout("damaged", &files());
    let memo = memo_at(&root, 1);
    let _ = run_with(&root, READERS, &memo);
    let path = root.join("target/census.memo");
    let whole = std::fs::read(&path).expect("the record");
    assert!(
        Record::decode(&whole).is_some(),
        "the record as written decodes"
    );
    let mut flipped = whole.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 1;
    std::fs::write(&path, &flipped).expect("damage");
    let (_, after_flip) = run_with(&root, READERS, &memo);
    std::fs::write(&path, &whole[..whole.len() - 1]).expect("cut");
    let (_, after_cut) = run_with(&root, READERS, &memo);
    let _ = std::fs::remove_dir_all(&root);
    for report in [after_flip, after_cut] {
        assert_eq!(
            report.no_record.as_deref(),
            Some("the kept record is not a whole record of this format"),
            "{report}"
        );
        assert!(report.reused.is_empty(), "{report}");
    }
}

/// Records one unit asking `ask` of the guards' corpus of `files`, applies `change`, and says
/// whether the next run reuses the unit, or why not.
fn reused_after(
    name: &str,
    files: &[(&str, &str)],
    ask: fn(&Corpus),
    change: impl FnOnce(&Path),
) -> Result<(), Rejudged> {
    let root = checkout(name, files);
    let memo = memo_at(&root, 1);
    let ran = |record: bool| {
        let (run, snapshot) = Run::start(&root, Some(&memo), NO_ENGINE);
        let source = OnceLock::from(Arc::clone(&snapshot));
        let walk = Walk {
            root: &root,
            engine: NO_ENGINE,
            source: &source,
            run: Some(&run),
        };
        let corpus: Arc<Corpus> = loader(&run.shared, || read_corpus(&walk)).expect("reads");
        let reused = loader(&run.shared, || {
            let reused = run.reusable("unit", walk.snapshot()).map(drop);
            if record {
                let (_, trace) = unit(&run.shared, || ask(&corpus));
                run.keep_judged(
                    "unit",
                    Rejudged::Unrecorded,
                    Some((trace, Verdict::Findings(Vec::new()))),
                );
            }
            reused
        });
        let report = run.finish(&snapshot);
        (reused, report)
    };
    let (_, first) = ran(true);
    assert!(first.not_kept.is_none(), "{first}");
    change(&root);
    let (reused, _) = ran(false);
    let _ = std::fs::remove_dir_all(&root);
    reused
}

fn sources() -> [(&'static str, &'static str); 2] {
    [(A, "fn a() {}\n"), (B, "fn b() {}\n")]
}

/// A mention question holds while the file's answer does, whatever else changed in it.
#[test]
fn a_mention_question_holds_while_its_answer_does() {
    let ask: fn(&Corpus) = |c| {
        let _ = c.get(A).expect("a").mentions(&["needle", "pin"]);
    };
    assert_eq!(
        reused_after("mention-kept", &sources(), ask, |r| write(
            r,
            A,
            "fn a() { 1 }\n"
        )),
        Ok(())
    );
    assert_eq!(
        reused_after("mention-lost", &sources(), ask, |r| write(
            r,
            A,
            "fn needle() {}\n"
        )),
        Err(Rejudged::Changed(format!("{A} changed what it mentions")))
    );
}

/// A content question holds while the file's bytes do.
#[test]
fn a_content_question_holds_while_the_bytes_do() {
    let ask: fn(&Corpus) = |c| {
        let _ = c.get(A).expect("a").lexed();
    };
    assert_eq!(
        reused_after("content-kept", &sources(), ask, |r| write(
            r,
            B,
            "fn b() { 2 }\n"
        )),
        Ok(())
    );
    assert_eq!(
        reused_after("content-lost", &sources(), ask, |r| write(
            r,
            A,
            "fn a() { 2 }\n"
        )),
        Err(Rejudged::Changed(format!("{A} changed")))
    );
}

/// A listing holds while the files under its prefixes do.
#[test]
fn a_listing_holds_while_its_files_do() {
    let ask: fn(&Corpus) = |c| {
        let _ = c.under(&["crates/x/"]).count();
    };
    assert_eq!(
        reused_after("listing-kept", &sources(), ask, |r| write(
            r,
            "crates/y/src/c.rs",
            ""
        )),
        Ok(())
    );
    assert_eq!(
        reused_after("listing-lost", &sources(), ask, |r| write(
            r,
            "crates/x/src/c.rs",
            ""
        )),
        Err(Rejudged::Changed(
            "the files under crates/x/ changed".to_string()
        ))
    );
}

/// Whether a file is in the corpus holds while it stays or stays away.
#[test]
fn a_presence_question_holds_while_the_file_stays_away() {
    const C: &str = "crates/x/src/c.rs";
    let ask: fn(&Corpus) = |c| {
        let _ = c.get(C);
    };
    assert_eq!(
        reused_after("has-kept", &sources(), ask, |r| write(
            r,
            "crates/x/src/d.rs",
            ""
        )),
        Ok(())
    );
    assert_eq!(
        reused_after("has-lost", &sources(), ask, |r| write(r, C, "")),
        Err(Rejudged::Changed(format!("{C} came or went")))
    );
}

/// A corpus read on a thread that carries no unit's scope keeps no record.
/// NEGATIVE CONTROL: the same read on a [`threads`] thread is the unit's question.
#[test]
fn a_corpus_read_off_the_units_threads_keeps_nothing() {
    let root = checkout("off-thread", &sources());
    let memo = memo_at(&root, 1);
    let ran = |inherit: bool| {
        let (run, snapshot) = Run::start(&root, Some(&memo), NO_ENGINE);
        let source = OnceLock::from(Arc::clone(&snapshot));
        let walk = Walk {
            root: &root,
            engine: NO_ENGINE,
            source: &source,
            run: Some(&run),
        };
        let corpus = loader(&run.shared, || read_corpus(&walk)).expect("reads");
        let (_, trace) = unit(&run.shared, || {
            if inherit {
                threads(|scope| scope.spawn(|| corpus.get(A).map(|f| f.raw().len())).join())
                    .expect("joins");
            } else {
                std::thread::scope(|scope| {
                    scope.spawn(|| corpus.get(A).map(|f| f.raw().len())).join()
                })
                .expect("joins");
            }
        });
        let asked = trace.content.len();
        run.keep_judged(
            "unit",
            Rejudged::Unrecorded,
            Some((trace, Verdict::Findings(Vec::new()))),
        );
        (asked, run.finish(&snapshot))
    };
    let (asked_inherited, inherited) = ran(true);
    let (_, raw) = ran(false);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        asked_inherited, 1,
        "the helper thread asked the unit's question"
    );
    assert!(inherited.not_kept.is_none(), "{inherited}");
    assert_eq!(
        raw.not_kept.as_deref(),
        Some("the memo's corpus was read off its units' threads"),
        "{raw}"
    );
}
