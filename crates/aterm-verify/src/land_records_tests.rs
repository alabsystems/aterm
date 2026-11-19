// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The verdict records a landing writes (`docs/DESIGN-landing-economy.md` slice 4), over scratch
//! repositories whose bare origin holds the journal, appended by [`crate::judged`] as the live
//! seams do: what is written when, signed by whom, kept where, and that a landing's exit and push
//! are the same whether its records were written or not. Each rule sits beside the control that
//! shows it bites; each control was also run as a mutant of the code it guards, named in the
//! test's doc, which made the test fail.

use super::*;
use crate::checks::Check;
use crate::event_log::{Finished, Resealed};
use crate::land_record::tests::{FINDING, NOW, planned as plan_of, row as fixture_row, signer};
use crate::land_record::{Entry, Origin, Signed, Signer, Word};
use crate::land_spool::Spool;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn a_spool_cleanup_failure_is_reported_without_losing_a_successful_append() {
    struct RefusedCleanup {
        dir: crate::land_spool::Dir,
        path: String,
    }
    impl Spool for RefusedCleanup {
        fn put(&self, record: &Signed) -> Result<(), String> {
            self.dir.put(record)
        }
        fn pending(&self) -> Result<crate::land_spool::Pending, String> {
            self.dir.pending()
        }
        fn done(&self, record: &Signed) -> Result<(), String> {
            if record.path == self.path {
                Err("controlled cleanup refusal".to_owned())
            } else {
                self.dir.done(record)
            }
        }
    }
    use crate::land_record::tests::{BASE, T, record};
    let world = world_of();
    let mac = Mac::new(&world, Some(7));
    let me = signer(7);
    let held = record(Check::Fmt, Word::Ok, T, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    let removed = record(Check::Lint, Word::Ok, T, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    mac.dir().put(&held).unwrap();
    mac.dir().put(&removed).unwrap();
    let mut records = mac.records();
    records.spool = Box::new(RefusedCleanup {
        dir: mac.dir(),
        path: held.path.clone(),
    });
    let mut said = Vec::new();
    records
        .publish(&world.work, &mut |line| said.push(line.to_owned()))
        .expect("cleanup cannot turn a successful append into an unwritten one");
    assert_eq!(filed(&world.work, T), ["fmt.ok", "lint.ok"]);
    assert_eq!(
        mac.dir().pending().unwrap().records.as_slice(),
        std::slice::from_ref(&held)
    );
    assert!(
        said.iter()
            .any(|line| line.contains(&held.path) && line.contains("controlled cleanup refusal")),
        "{said:?}"
    );
}

/// A world whose clones can write the journal (its appends are commits).
fn world_of() -> World {
    let world = world();
    for clone in [&world.work, &world.rival] {
        git(clone, &["config", "user.name", "Andrew Yates"]);
        git(clone, &["config", "user.email", "test@example.invalid"]);
    }
    world
}

/// One Mac's side of the records in a scripted landing: the journal on the world's scratch
/// origin, a spool beside it, a fixture node identity (`seed`; `None`: none) and a host name.
struct Mac {
    seed: Option<u8>,
    spool: PathBuf,
    /// While set, each append fails.
    unwritten: Rc<Cell<bool>>,
}

impl Mac {
    fn new(world: &World, seed: Option<u8>) -> Self {
        Self {
            seed,
            spool: world.origin.with_file_name("spool"),
            unwritten: Rc::default(),
        }
    }

    /// Its records, appended to the scratch origin's journal at [`NOW`].
    fn records(&self) -> Records<'static> {
        let unwritten = self.unwritten.clone();
        Records {
            append: Box::new(move |root: &Path, records: &[Signed], at: u64| {
                if unwritten.get() {
                    return Err("the scratch journal refuses appends".to_owned());
                }
                crate::judged::append_land(root, REMOTE, records, at, crate::judged::deadline())
                    .map(drop)
            }),
            spool: Box::new(self.dir()),
            signer: self
                .seed
                .map(|seed| Box::new(signer(seed)) as Box<dyn Signer>),
            host: "m45".to_owned(),
            now: Box::new(|| NOW),
            run: crate::land_run::tests::run(),
            hosting: Box::new(crate::hosting::Unhosted),
            interrupt: Box::new(|| None),
            tally: records::Tally::default(),
            lock: None,
        }
    }

    fn dir(&self) -> crate::land_spool::Dir {
        crate::land_spool::Dir::at(&self.spool)
    }

    /// The verdict records its spool holds, by `<check>.<word>`, sorted.
    fn spooled(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .dir()
            .pending()
            .expect("the spool")
            .records
            .iter()
            .filter(|signed| !signed.path.starts_with(crate::land_run::DIR))
            .map(|signed| named(&signed.path))
            .collect();
        names.sort();
        names
    }
}

/// `<check>.<word>` of a record's journal path.
fn named(path: &str) -> String {
    let file = path.rsplit('/').next().expect("a file");
    let parts: Vec<&str> = file.split('.').collect();
    format!("{}.{}", parts[0], parts[2])
}

/// The records the journal on the origin files under `tree`.
fn journal(root: &Path, tree: &str) -> Vec<Entry> {
    let head = crate::judged::head(root, REMOTE, crate::judged::deadline()).expect("the head");
    crate::judged::land_records(root, head.as_deref(), &[tree], crate::judged::deadline())
        .expect("the records")
}

/// [`journal`]'s records by `<check>.<word>`, sorted.
fn filed(root: &Path, tree: &str) -> Vec<String> {
    let mut names: Vec<String> = journal(root, tree)
        .iter()
        .map(|entry| named(&entry.path))
        .collect();
    names.sort();
    names
}

/// Append what a landing that pushed nothing left in `mac`'s spool, as the clone's next landing
/// appends it ([`Records::publish`]), once the journal is shown to hold none of `tree`'s
/// records: the run deferred them ([`records::finish`]).
fn append_kept(world: &World, mac: &Mac, tree: &str) {
    assert_eq!(
        filed(&world.work, tree),
        Vec::<String>::new(),
        "a run that pushed nothing appends none of its records"
    );
    let mut said = Vec::new();
    mac.records()
        .publish(&world.work, &mut |line| said.push(line.to_owned()))
        .expect("the next landing appends what the spool kept");
    assert!(said.is_empty(), "{said:#?}");
    assert!(mac.spooled().is_empty(), "the append held them");
}

/// What a scripted landing did: its exit, what it said, and each judgment's base and tree.
struct Landed {
    code: i32,
    said: Vec<String>,
    judged: Vec<(String, String)>,
}

/// Land `root`'s branch (its crates `a`) with `records`, each judgment run by `judge`: the base,
/// the tree and the poll. A shared path reaches every crate, so no round answers a moved main.
fn land_on(
    root: &Path,
    records: Records<'_>,
    judge: impl FnMut(&str, &str, Poll<'_>) -> Ran,
) -> Landed {
    land_heard(root, records, judge, &|_| {})
}

/// [`land_on`], each line the landing says also told to `heard` as it is said.
fn land_heard(
    root: &Path,
    records: Records<'_>,
    judge: impl FnMut(&str, &str, Poll<'_>) -> Ran,
    heard: &dyn Fn(&str),
) -> Landed {
    let branch = crates(&["a"]);
    let reach = whole_reach(&branch);
    land_rounds_heard(
        root,
        records,
        &reach,
        judge,
        |_, _| panic!("no round was expected"),
        heard,
    )
}

/// [`land_on`] whose shared paths reach what `reach` says (its argument the paths, or `None` for
/// the branch's own change), each round run by `round`: the base (main's tip) and the merge's
/// tree.
fn land_rounds(
    root: &Path,
    records: Records<'_>,
    reach: &dyn Fn(Option<&[String]>) -> Reach,
    judge: impl FnMut(&str, &str, Poll<'_>) -> Ran,
    round: impl FnMut(&str, &str) -> Judged,
) -> Landed {
    land_rounds_heard(root, records, reach, judge, round, &|_| {})
}

/// [`land_rounds`], each line the landing says also told to `heard` as it is said.
fn land_rounds_heard(
    root: &Path,
    records: Records<'_>,
    reach: &dyn Fn(Option<&[String]>) -> Reach,
    mut judge: impl FnMut(&str, &str, Poll<'_>) -> Ran,
    mut round: impl FnMut(&str, &str) -> Judged,
    heard: &dyn Fn(&str),
) -> Landed {
    let said = RefCell::new(Vec::new());
    let judged = RefCell::new(Vec::new());
    let code = land(
        root,
        &own(),
        &mut Seams {
            git: &git_cmd,
            autofix: &mut |_: &Path, _: &[String]| Ok(()),
            verifier: &mut |_: &Path| Ok(own_image()),
            judge: &mut |at: &Path, _: &Path, base: &str, poll: Poll<'_>| {
                let tree = git(at, &["rev-parse", "HEAD^{tree}"]);
                judged.borrow_mut().push((base.to_owned(), tree.clone()));
                judge(base, &tree, poll)
            },
            reach: &|_: &Path, _: &str, paths: Option<&[String]>| reach(paths),
            records,
            recheck: &mut |at: &Path, _: &Path, base: &str, _: &Delta| {
                round(base, &git(at, &["rev-parse", "HEAD^{tree}"]))
            },
            hand_over: &mut |_: &Image, _: &Path, _: &Path| panic!("no handoff was expected"),
            say: &mut |line: &str| {
                heard(line);
                said.borrow_mut().push(line.to_owned());
            },
            remote: &live_remote,
            pause: &no_retry,
        },
    );
    Landed {
        code,
        said: said.into_inner(),
        judged: judged.into_inner(),
    }
}

/// The word `words` names for `check`, `ok` where none.
fn word_of(words: &[(Check, Word)], check: Check) -> Word {
    words
        .iter()
        .find(|(named, _)| *named == check)
        .map_or(Word::Ok, |(_, word)| *word)
}

/// The run's exit its rows give.
fn code_of(rows: &[CheckRow]) -> i32 {
    if rows.iter().any(|row| row.word == Word::Fail.spelled()) {
        exit::FAILED
    } else if rows.iter().any(|row| row.word == Word::NotRun.spelled()) {
        exit::COULD_NOT_RUN
    } else {
        exit::PASS
    }
}

/// A run with invocation `id` of `tree` at `base`, as T's own verifier makes it: each check of
/// [`plan_of`]'s plan says its word in `words`; its record whole and sealed through.
fn script(id: &str, (tree, base): (&str, &str), words: &[(Check, Word)]) -> Judged {
    let planned = plan_of(tree, base);
    let rows: Vec<CheckRow> = planned
        .required
        .iter()
        .map(|row| fixture_row(row.check, word_of(words, row.check), &planned))
        .collect();
    let code = code_of(&rows);
    let resealed = Resealed {
        seals: planned
            .required
            .iter()
            .map(|row| (row.check, row.seal.clone()))
            .collect(),
        local: planned.local.clone(),
        moved: Vec::new(),
        sealed_through: true,
    };
    Judged {
        code,
        finished: Some(Finished {
            code,
            checks: rows
                .iter()
                .map(|row| (row.check.clone(), row.word.clone()))
                .collect(),
        }),
        checks: rows,
        secs: 600,
        planned: Some(Box::new(planned)),
        resealed: Some(Box::new(resealed)),
        invocation: Some(id.to_owned()),
        reds: Vec::new(),
        kept: None,
    }
}

/// A judge that runs [`script`] with `words` and ends: a run named `id`.
fn judging<'w>(
    id: &'w str,
    words: &'w [(Check, Word)],
) -> impl FnMut(&str, &str, Poll<'_>) -> Ran + 'w {
    move |base, tree, _| Ran::Ended(script(id, (tree, base), words))
}

/// THE RECORDS OF A JUDGMENT: a whole run records one signed record per check of its plan, under
/// the tree it judged and the main commit it merged, each reading back with its run and its key
/// the plan's, and `verify land records` lists them; the landing lands as without them. A run
/// whose record is not whole (no `run-finished`) records its FAIL alone: its passes name a run
/// that did not end as its rows say; it pushed nothing, so its record waits in the clone's spool
/// for the next landing's append ([`append_kept`]). NEGATIVE CONTROL, run as a mutant that made
/// this test fail:
/// `end` writing every row whatever the run's record (the not-whole run filed `fmt.ok`).
#[test]
fn a_whole_run_records_every_check_and_one_not_whole_its_fails_alone() {
    let world = world_of();
    commit(&world.work, "a/x.txt", "a2\n");
    let mac = Mac::new(&world, Some(1));
    let landing = land_on(&world.work, mac.records(), judging("abab0001", &[]));
    assert_eq!(landing.code, exit::PASS, "{:#?}", landing.said);
    let (base, tree) = landing.judged[0].clone();
    assert_eq!(
        filed(&world.work, &tree),
        ["cells.ok", "fmt.ok", "lint.ok", "test.ok"]
    );
    let plan = plan_of(&tree, &base);
    let entries = journal(&world.work, &tree);
    for entry in &entries {
        let read = entry.body.as_ref().expect("it reads");
        let record = &read.record;
        let Some(crate::checks::Word::Check(check)) = crate::checks::lookup(&record.check) else {
            panic!("{} is no check", record.check);
        };
        assert_eq!(record.key, plan.row(check).expect("planned").key);
        assert_eq!(
            (record.base.as_str(), record.run.id.as_str()),
            (base.as_str(), "abab0001")
        );
        assert_eq!(
            (record.run.whole, record.run.sealed_through),
            (Some(true), Some(true))
        );
        assert_eq!(
            read.signature.as_ref().map(|s| s.key),
            Some(signer(1).key())
        );
    }
    let lines = records::listing(&tree, &entries, Some(signer(1).key()), NOW, false);
    assert_eq!(lines[0], format!("{tree}: 4 records"), "{lines:#?}");
    assert!(
        lines[1..]
            .iter()
            .all(|line| line.contains("signed by this Mac")
                && line.contains(&format!("base={base}"))
                && line.contains("whole=yes")),
        "{lines:#?}"
    );
    assert!(
        mac.spooled().is_empty(),
        "an append that held them clears the spool"
    );

    let world = world_of();
    commit(&world.work, "a/x.txt", "a2\n");
    let mac = Mac::new(&world, Some(1));
    let landing = land_on(&world.work, mac.records(), |base, tree, _| {
        let mut judged = script("abab0002", (tree, base), &[(Check::Lint, Word::Fail)]);
        judged.finished = None;
        Ran::Ended(judged)
    });
    assert_eq!(landing.code, exit::FAILED, "{:#?}", landing.said);
    let (_, tree) = &landing.judged[0];
    assert_eq!(mac.spooled(), ["lint.FAIL"]);
    append_kept(&world, &mac, tree);
    assert_eq!(filed(&world.work, tree), ["lint.FAIL"]);
    let record = journal(&world.work, tree)
        .remove(0)
        .body
        .expect("it reads")
        .record;
    assert_eq!(record.run.whole, Some(false));
    assert_eq!(record.findings, [FINDING]);
}

/// A run a poll interrupted writes no record, and neither does a run whose checkout moved before
/// it ended (it judged what the checkout no longer holds). NEGATIVE CONTROL: the judgment the
/// interrupt led to, which ended unmoved, records one record per check of its plan. Mutant run:
/// `records::record` called before the `moved_from` check wrote the moved run's four records,
/// its fmt FAIL among them.
#[test]
fn an_interrupted_or_moved_run_writes_nothing() {
    let world = world_of();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let first = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
    let mac = Mac::new(&world, Some(1));
    let mut n = 0;
    let landing = land_on(&world.work, mac.records(), |base, tree, poll: Poll<'_>| {
        n += 1;
        if n == 1 {
            rival_moves_line(&world, 4, "moved");
            assert!(
                poll(&mut || false),
                "a main over the branch's file interrupts"
            );
            return Ran::Interrupted {
                secs: 1,
                queued: false,
            };
        }
        Ran::Ended(script("cdcd0002", (tree, base), &[]))
    });
    assert_eq!(landing.code, exit::PASS, "{:#?}", landing.said);
    assert_eq!(landing.judged.len(), 2);
    assert_eq!(filed(&world.work, &first), Vec::<String>::new());
    let (_, merged) = &landing.judged[1];
    assert_eq!(
        filed(&world.work, merged),
        ["cells.ok", "fmt.ok", "lint.ok", "test.ok"],
        "the control: an ended, unmoved run writes one record per check"
    );

    let world = world_of();
    commit(&world.work, "a/x.txt", "a2\n");
    let mac = Mac::new(&world, Some(1));
    let landing = land_on(&world.work, mac.records(), |base, tree, _| {
        let judged = script("cdcd0003", (tree, base), &[(Check::Fmt, Word::Fail)]);
        commit(&world.work, "a/x.txt", "moved during the run\n");
        Ran::Ended(judged)
    });
    assert_eq!(landing.code, exit::COULD_NOT_RUN, "{:#?}", landing.said);
    let (_, tree) = &landing.judged[0];
    assert_eq!(filed(&world.work, tree), Vec::<String>::new());
    assert!(mac.spooled().is_empty());
}

/// THE RECORDS OF A ROUND: a main that moved into a file the branch changed, by a change whose
/// reach holds one of the branch's crates and not the other, is answered by a round once the
/// judgment passed, and the round's verdicts are recorded under the merge's tree and main's
/// tip, as the merge's own run: its FAIL with its passes, beside the judgment's own records of
/// the tree it judged. The landing exits as the round's red says, and its records reach the
/// journal with the clone's next landing ([`append_kept`]). NEGATIVE CONTROL: the judgment's
/// records of its own tree. Mutant run: `recheck` without its `records::record` filed nothing
/// under the merge's tree.
#[test]
fn a_rounds_verdicts_are_recorded_under_the_merges_tree() {
    let world = world_of();
    commit(&world.work, "shared.txt", &SHARED.replacen('1', "one", 1));
    let mac = Mac::new(&world, Some(1));
    let tip = RefCell::new(String::new());
    let rounds = RefCell::new(Vec::new());
    let landing = land_rounds(
        &world.work,
        mac.records(),
        &|paths| match paths {
            Some(_) => reaches(&["b"]),
            None => reaches(&["a", "b"]),
        },
        |base, tree, poll: Poll<'_>| {
            *tip.borrow_mut() = rival_moves_line(&world, 4, "moved");
            assert!(
                !poll(&mut || false),
                "a main a round answers does not interrupt the judgment"
            );
            Ran::Ended(script("cdcd0004", (tree, base), &[]))
        },
        |base, tree| {
            rounds.borrow_mut().push((base.to_owned(), tree.to_owned()));
            script("cdcd0005", (tree, base), &[(Check::Lint, Word::Fail)])
        },
    );
    assert_eq!(landing.code, exit::FAILED, "{:#?}", landing.said);
    let tip = tip.into_inner();
    let rounds = rounds.into_inner();
    let [(base, merge)] = rounds.as_slice() else {
        panic!("one round: {rounds:?} {:#?}", landing.said);
    };
    assert_eq!(base, &tip, "the round runs since main's tip");
    let [(_, judged)] = landing.judged.as_slice() else {
        panic!("one judgment: {:#?}", landing.said);
    };
    assert_ne!(merge, judged);
    append_kept(&world, &mac, merge);
    assert_eq!(
        filed(&world.work, judged),
        ["cells.ok", "fmt.ok", "lint.ok", "test.ok"],
        "the control: the judgment's own records of the tree it judged"
    );
    assert_eq!(
        filed(&world.work, merge),
        ["cells.ok", "fmt.ok", "lint.FAIL", "test.ok"]
    );
    let head = git(&world.work, &["rev-parse", "HEAD"]);
    for entry in journal(&world.work, merge) {
        let record = entry.body.expect("it reads").record;
        assert_eq!(
            (
                record.base.as_str(),
                record.commit.as_str(),
                record.run.id.as_str()
            ),
            (tip.as_str(), head.as_str(), "cdcd0005")
        );
    }
}

/// A RECORD NO APPEND HELD IS KEPT IN THE CLONE, AND CHANGES NOTHING THE LANDING DOES: with the
/// journal refusing appends the landing still lands, says its records were kept in this clone,
/// and its spool holds them; the next landing's one append carries them beside its own, and the
/// spool is empty again. NEGATIVE CONTROL: with no spool the line says the records are kept
/// nowhere, and the landing still lands. Mutant run: `publish` appending only this run's records
/// left the first tree's out of the journal.
#[test]
fn a_record_no_append_held_stays_spooled_until_the_next_landing_appends_it() {
    let world = world_of();
    commit(&world.work, "a/x.txt", "a2\n");
    let mac = Mac::new(&world, Some(1));
    mac.unwritten.set(true);
    let landing = land_on(&world.work, mac.records(), judging("efef0001", &[]));
    assert_eq!(landing.code, exit::PASS, "{:#?}", landing.said);
    let (_, first) = landing.judged[0].clone();
    assert!(
        landing
            .said
            .iter()
            .any(|line| line.contains("(the scratch journal refuses appends)")),
        "the refusal is said: {:#?}",
        landing.said
    );
    assert_eq!(mac.spooled(), ["cells.ok", "fmt.ok", "lint.ok", "test.ok"]);
    assert_eq!(filed(&world.work, &first), Vec::<String>::new());

    mac.unwritten.set(false);
    commit(&world.work, "a/x.txt", "a3\n");
    let landing = land_on(&world.work, mac.records(), judging("efef0002", &[]));
    assert_eq!(landing.code, exit::PASS, "{:#?}", landing.said);
    let (_, second) = &landing.judged[0];
    assert_ne!(*second, first);
    for tree in [&first, second] {
        assert_eq!(
            filed(&world.work, tree),
            ["cells.ok", "fmt.ok", "lint.ok", "test.ok"],
            "{tree}"
        );
    }
    assert!(mac.spooled().is_empty());

    let world = world_of();
    commit(&world.work, "a/x.txt", "a2\n");
    let mut records = Mac::new(&world, Some(1)).records();
    records.append = Box::new(|_: &Path, _: &[Signed], _: u64| Err("refused".to_owned()));
    records.spool = Box::new(records::Unavailable("no spool here".to_owned()));
    let landing = land_on(&world.work, records, judging("efef0003", &[]));
    assert_eq!(landing.code, exit::PASS, "{:#?}", landing.said);
    assert!(
        landing
            .said
            .iter()
            .any(|line| line.contains("(refused)") && line.contains("(no spool here)")),
        "the line names both the refusal and why the spool keeps none: {:#?}",
        landing.said
    );
}

/// A conflict returns after keeping its signed records, without starting optional journal
/// work. A fresh controller reads those exact bytes from disk; its next successful landing
/// publishes them beside its own records and clears the spool.
#[test]
fn a_conflict_keeps_its_records_for_the_next_landing_without_appending() {
    let world = world_of();
    let own = commit(&world.work, "shared.txt", &SHARED.replacen('4', "mine", 1));
    let mac = Mac::new(&world, Some(1));
    let mut records = mac.records();
    let first_run = records.run.id.clone();
    records.append = Box::new(|_, _, _| panic!("a kept conflict must not start an append"));
    let mut judge = judging("cafe0001", &[]);
    let stopped = land_on(&world.work, records, |base, tree, poll| {
        rival_moves(&world, "shared.txt", &SHARED.replacen('4', "theirs", 1));
        judge(base, tree, poll)
    });
    assert_eq!(stopped.code, exit::CONFLICT, "{:#?}", stopped.said);
    assert_eq!(git(&world.work, &["rev-parse", "HEAD"]), own);
    assert!(git(&world.work, &["status", "--porcelain"]).contains("UU shared.txt"));
    assert!(kept_record(&world).is_some());
    assert!(
        crate::judged::head(&world.work, REMOTE, crate::judged::deadline())
            .unwrap()
            .is_none()
    );

    let reopened = crate::land_spool::Dir::at(mac.spool.clone());
    let pending = reopened
        .pending()
        .expect("the records survive their controller");
    assert!(pending.unreadable.is_empty());
    assert_eq!(mac.spooled(), ["cells.ok", "fmt.ok", "lint.ok", "test.ok"]);
    let mut end = None;
    for signed in &pending.records {
        let signature = if signed.path.starts_with(crate::land_run::DIR) {
            let read = crate::land_run::Entry::read(&signed.path, &signed.bytes, Origin::Spool)
                .body
                .expect("the run record and its signature verify");
            if read.record.kind == "end" {
                end = Some(read.record.clone());
            }
            read.signature
        } else {
            Entry::read(&signed.path, &signed.bytes, Origin::Spool)
                .body
                .expect("the verdict and its signature verify")
                .signature
        };
        assert_eq!(
            signature.map(|signature| signature.key),
            Some(signer(1).key())
        );
    }
    assert_eq!(
        end.expect("the conflict has an end record").exit,
        Some(exit::CONFLICT)
    );

    super::push::resolve(&world, &[("shared.txt", "resolved\n")]);
    let mut next = mac.records();
    next.run.id = "ffffffffffffffffffffffffffffffff".into();
    assert_ne!(next.run.id, first_run);
    let branch = crates(&["a"]);
    let reach = whole_reach(&branch);
    let landed = land_rounds(
        &world.work,
        next,
        &reach,
        |_, _, _| panic!("the kept merge needs its round, not another full judgment"),
        |_, _| super::push::round_green(),
    );
    assert_eq!(landed.code, exit::PASS, "{:#?}", landed.said);
    let head = crate::judged::head(&world.work, REMOTE, crate::judged::deadline())
        .unwrap()
        .expect("the next landing delivered the spool");
    for signed in &pending.records {
        let output = git_cmd(&world.work)
            .args(["show", &format!("{head}:{}", signed.path)])
            .output()
            .expect("read the appended record");
        assert!(output.status.success());
        assert_eq!(output.stdout, signed.bytes, "{}", signed.path);
    }
    let remaining = reopened.pending().unwrap();
    assert!(remaining.records.is_empty());
    assert!(remaining.unreadable.is_empty());
}

/// A RED LANDING KEEPS ITS RECORDS IN THE CLONE AND LEAVES THE CHECKOUT FREE AT ITS VERDICT: a
/// run that pushed nothing (here a FAIL) starts no append; its signed records, its end
/// included, stay in the clone's spool, said after its verdict, and the checkout's landing lock
/// it held is free by the time it says so, while the run still lives, so the fix lands at once.
/// That landing says its verdict, then lets its lock go, then appends: its one append carries
/// the red run's records beside its own, and the spool is empty again.
///
/// NEGATIVE CONTROLS, each run once against a broken copy of `land_records.rs` or `land.rs`
/// (restored with cp, then touch), which made this test fail:
/// - `finish` returning `Delivery::Publish(None)` for every run that took no interrupt (the
///   append of every exit, as before): `a run that pushed nothing starts no append`;
/// - `deliver` letting the lock go only after its append: `the landing let its lock go before
///   it appends: Err("another landing runs in …: it holds …/.git/aterm-land.lock until it ends
///   …")`;
/// - `deliver` letting the lock go only in its `Delivery::Publish` arm, so a kept run holds it
///   through its delivery: `the red run let its lock go before it said where its records went`,
///   `left: Some(Err("another landing runs in …"))`;
/// - `concluded` delivering before `say_verdict`: `assertion `left == right` failed` on the
///   line after the red run's verdict (the records' line came before it).
#[test]
fn a_red_landing_keeps_its_records_and_leaves_the_checkout_free_for_the_next_land() {
    let world = world_of();
    commit(&world.work, "a/x.txt", "a2\n");
    let mac = Mac::new(&world, Some(1));
    let checkout = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let mut red = mac.records();
    red.run.id = "fa11".repeat(8);
    red.append = Box::new(|_, _, _| panic!("a run that pushed nothing starts no append"));
    red.lock = Some(checkout.hold().expect("the checkout's landing lock"));
    // The checkout's landing as the red run's records line is said, while the run (and the
    // records that held its lock) still lives.
    let free_at_kept = RefCell::new(None);
    let landing = land_heard(
        &world.work,
        red,
        judging("fa110001", &[(Check::Lint, Word::Fail)]),
        &|line| {
            if line.starts_with("verify land: this run's records are kept in this clone") {
                *free_at_kept.borrow_mut() = Some(checkout.hold().map(drop));
            }
        },
    );
    assert_eq!(landing.code, exit::FAILED, "{:#?}", landing.said);
    assert_eq!(
        free_at_kept.into_inner(),
        Some(Ok(())),
        "the red run let its lock go before it said where its records went: {:#?}",
        landing.said
    );
    let (_, red_tree) = landing.judged[0].clone();
    let verdict = landing
        .said
        .iter()
        .position(|line| line.starts_with("verify land: VERDICT — NOT LANDED"))
        .expect("the verdict");
    assert_eq!(
        landing.said[verdict + 1..].last().map(String::as_str),
        Some(
            format!(
                "verify land: this run's records are kept in this clone; {}",
                records::WHO_APPENDS
            )
            .as_str()
        ),
        "{:#?}",
        landing.said
    );
    assert_eq!(
        mac.spooled(),
        ["cells.ok", "fmt.ok", "lint.FAIL", "test.ok"]
    );
    let red_end = mac
        .dir()
        .pending()
        .unwrap()
        .records
        .into_iter()
        .find(|signed| {
            crate::land_run::Filed::parse(&signed.path).is_ok_and(|filed| {
                filed.kind == crate::land_run::Kind::End && signed.path.contains(&"fa11".repeat(8))
            })
        })
        .expect("the red run's end is kept");
    assert!(
        crate::judged::head(&world.work, REMOTE, crate::judged::deadline())
            .unwrap()
            .is_none(),
        "nothing reached the journal"
    );

    let held = checkout
        .hold()
        .expect("the red run left the checkout's landing free");
    commit(&world.work, "a/x.txt", "a3\n");
    let said = RefCell::new(Vec::<String>::new());
    let mut fixed = mac.records();
    fixed.run.id = "f1fe".repeat(8);
    fixed.lock = Some(held);
    let mut append = fixed.append;
    let (heard, free) = (&said, &checkout);
    fixed.append = Box::new(move |root, signed, at| {
        assert!(
            heard
                .borrow()
                .iter()
                .any(|line| line.starts_with("verify land: VERDICT — LANDED")),
            "the verdict is said before the append: {:#?}",
            heard.borrow()
        );
        let free = free.hold().map(drop);
        assert!(
            free.is_ok(),
            "the landing let its lock go before it appends: {free:?}"
        );
        append(root, signed, at)
    });
    let landed = land_heard(&world.work, fixed, judging("f1fe0001", &[]), &|line| {
        said.borrow_mut().push(line.to_owned())
    });
    assert_eq!(landed.code, exit::PASS, "{:#?}", landed.said);
    let (_, fixed_tree) = &landed.judged[0];
    assert_ne!(fixed_tree, &red_tree);
    for tree in [&red_tree, fixed_tree] {
        let want: &[&str] = if *tree == red_tree {
            &["cells.ok", "fmt.ok", "lint.FAIL", "test.ok"]
        } else {
            &["cells.ok", "fmt.ok", "lint.ok", "test.ok"]
        };
        assert_eq!(filed(&world.work, tree), want, "{tree}");
    }
    let head = crate::judged::head(&world.work, REMOTE, crate::judged::deadline())
        .unwrap()
        .expect("the journal");
    let output = git_cmd(&world.work)
        .args(["show", &format!("{head}:{}", red_end.path)])
        .output()
        .unwrap();
    assert!(output.status.success(), "the red run's end is appended");
    assert_eq!(output.stdout, red_end.bytes);
    assert!(mac.dir().pending().unwrap().records.is_empty());
}

/// A RUN AN INTERRUPT TOOK LETS THE CHECKOUT GO BEFORE IT SAYS ITS RECORDS ARE KEPT: its end
/// records the signal and stays in the clone's spool, no append starts, and the checkout's
/// landing lock the run held is free by the time the line that says so is said, while the run
/// (and its records) still lives.
///
/// NEGATIVE CONTROL, run once against a broken copy of `land_records.rs` (restored with cp, then
/// touch), which made this test fail: `deliver` letting the lock go only in its
/// `Delivery::Publish` arm, so a kept or interrupted run holds it through its delivery: `the run
/// let its lock go before it said where its records went`, `left: Some(Err("another landing runs
/// in …"))`.
#[test]
fn an_interrupted_run_lets_the_checkout_go_before_it_says_its_records_are_kept() {
    let world = world_of();
    let mac = Mac::new(&world, Some(1));
    let checkout = Git {
        root: &world.work,
        make: &git_cmd,
    };
    let mut records = mac.records();
    records.append = Box::new(|_, _, _| panic!("a run an interrupt took starts no append"));
    records.interrupt = Box::new(|| Some(15));
    records.lock = Some(checkout.hold().expect("the checkout's landing lock"));
    let mut said = Vec::new();
    let delivery = records::finish(
        &checkout,
        &mut records,
        &mut |line| said.push(line.to_owned()),
        exit::COULD_NOT_RUN,
    );
    assert_eq!(delivery, records::Delivery::Interrupted(15), "{said:#?}");
    let mut free = None;
    records::deliver(
        &world.work,
        &mut records,
        &mut |line| {
            if line
                .starts_with("verify land: interrupted by signal 15: this run's records are kept")
            {
                free = Some(checkout.hold().map(drop));
            }
            said.push(line.to_owned());
        },
        delivery,
    );
    assert_eq!(
        free,
        Some(Ok(())),
        "the run let its lock go before it said where its records went: {said:#?}"
    );
    let end = mac
        .dir()
        .pending()
        .unwrap()
        .records
        .into_iter()
        .find(|signed| {
            crate::land_run::Filed::parse(&signed.path)
                .is_ok_and(|filed| filed.kind == crate::land_run::Kind::End)
        })
        .expect("the interrupted run's end is kept");
    let entry = crate::land_run::Entry::read(&end.path, &end.bytes, Origin::Spool);
    assert_eq!(
        entry.body.expect("the end reads").record.signal,
        Some(15),
        "the end records the signal"
    );
    assert!(
        crate::judged::head(&world.work, REMOTE, crate::judged::deadline())
            .unwrap()
            .is_none(),
        "nothing reached the journal"
    );
}

/// Deferral cannot discard records when the end cannot be saved or durable custody cannot be
/// confirmed: publication rescues them and preserves the conflict exit. An end record that
/// cannot be formed must not produce a vacuous assurance that the run was kept.
#[test]
fn a_conflict_rescues_records_when_local_custody_is_not_confirmed() {
    struct RefusedCustody {
        dir: crate::land_spool::Dir,
        refuse_end: bool,
        flushed: Rc<Cell<bool>>,
    }
    impl Spool for RefusedCustody {
        fn put(&self, record: &Signed) -> Result<(), String> {
            if self.refuse_end
                && crate::land_run::Filed::parse(&record.path)
                    .is_ok_and(|filed| filed.kind == crate::land_run::Kind::End)
            {
                Err("controlled end persistence refusal".into())
            } else {
                self.dir.put(record)
            }
        }
        fn flush(&self) -> Result<(), String> {
            self.flushed.set(true);
            Err("controlled durability confirmation refusal".into())
        }
        fn pending(&self) -> Result<crate::land_spool::Pending, String> {
            self.dir.pending()
        }
        fn done(&self, record: &Signed) -> Result<(), String> {
            self.dir.done(record)
        }
    }
    for refuse_end in [true, false] {
        let world = world_of();
        commit(&world.work, "shared.txt", &SHARED.replacen('4', "mine", 1));
        let mac = Mac::new(&world, Some(1));
        let mut records = mac.records();
        let flushed = Rc::new(Cell::new(false));
        records.spool = Box::new(RefusedCustody {
            dir: mac.dir(),
            refuse_end,
            flushed: flushed.clone(),
        });
        let delivered = Rc::new(RefCell::new(Vec::new()));
        let observed = delivered.clone();
        let mut append = records.append;
        records.append = Box::new(move |root, signed, at| {
            *observed.borrow_mut() = signed.to_vec();
            append(root, signed, at)
        });
        let mut judge = judging("cafe0003", &[]);
        let stopped = land_on(&world.work, records, |base, tree, poll| {
            rival_moves(&world, "shared.txt", &SHARED.replacen('4', "theirs", 1));
            judge(base, tree, poll)
        });
        assert_eq!(stopped.code, exit::CONFLICT, "{:#?}", stopped.said);
        if !refuse_end {
            assert!(flushed.get(), "the confirmation refusal was exercised");
        }
        let end = delivered
            .borrow()
            .iter()
            .find(|signed| {
                crate::land_run::Filed::parse(&signed.path)
                    .is_ok_and(|filed| filed.kind == crate::land_run::Kind::End)
            })
            .expect("the append rescues the end despite unconfirmed custody")
            .clone();
        let read = crate::land_run::Entry::read(&end.path, &end.bytes, Origin::Journal)
            .body
            .expect("the rescued end reads");
        assert_eq!(read.record.exit, Some(exit::CONFLICT));
        let head = crate::judged::head(&world.work, REMOTE, crate::judged::deadline())
            .unwrap()
            .expect("the rescue reached the journal");
        let output = git_cmd(&world.work)
            .args(["show", &format!("{head}:{}", end.path)])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, end.bytes);
        assert!(mac.dir().pending().unwrap().records.is_empty());

        if !refuse_end {
            let mut absent = mac.records();
            absent.run.id = "not-a-run-id".into();
            absent.signer = None;
            let mut said = Vec::new();
            let delivery = records::finish(
                &Git {
                    root: &world.work,
                    make: &git_cmd,
                },
                &mut absent,
                &mut |line| said.push(line.to_owned()),
                exit::CONFLICT,
            );
            records::deliver(
                &world.work,
                &mut absent,
                &mut |line| said.push(line.to_owned()),
                delivery,
            );
            assert!(
                said.iter()
                    .any(|line| line.contains("end record was not written")),
                "{said:#?}"
            );
            assert!(
                !said.iter().any(|line| line.contains(records::WHO_APPENDS)),
                "{said:#?}"
            );
        }
    }
}

/// A verifier older than `run-planned` (its rows carry no key) has its FAIL rows recorded
/// unkeyed, under the controller's key, and no pass, read from the journal once the clone's next
/// landing appended them ([`append_kept`]). NEGATIVE CONTROL: the same run with
/// `run-planned` records every row keyed, as a whole run's are. Mutant run: dropping `end`'s
/// keyed filter wrote the old run's passes.
#[test]
fn an_older_verifier_without_run_planned_writes_unkeyed_fails_only() {
    for planned in [false, true] {
        let world = world_of();
        commit(&world.work, "a/x.txt", "a2\n");
        let mac = Mac::new(&world, Some(1));
        let landing = land_on(&world.work, mac.records(), |base, tree, _| {
            if planned {
                return Ran::Ended(script(
                    "baba0001",
                    (tree, base),
                    &[(Check::Lint, Word::Fail)],
                ));
            }
            Ran::Ended(ran(
                exit::FAILED,
                vec![
                    row("fmt", "ok"),
                    red("lint", &[FINDING]),
                    row("test", "ok"),
                    row("cells", "ok"),
                ],
                1,
            ))
        });
        assert_eq!(landing.code, exit::FAILED, "{:#?}", landing.said);
        let (_, tree) = &landing.judged[0];
        append_kept(&world, &mac, tree);
        let entries = journal(&world.work, tree);
        let mut names: Vec<String> = entries.iter().map(|entry| named(&entry.path)).collect();
        names.sort();
        if planned {
            assert_eq!(names, ["cells.ok", "fmt.ok", "lint.FAIL", "test.ok"]);
        } else {
            assert_eq!(names, ["lint.FAIL"]);
        }
        let keyed = entries
            .iter()
            .all(|entry| entry.body.as_ref().is_ok_and(|read| read.record.keyed()));
        assert_eq!(keyed, planned, "{entries:#?}");
    }
}

/// This Mac's node seed stays in the controller: the nested verifier's command (its words and
/// its environment) names neither the seed nor the identity's file, and the spool's records
/// carry no seed, its run records included. NEGATIVE CONTROL: the controller's signer signed
/// them. Mutant run: `record` spooling the records unsigned made the control fail.
#[test]
fn the_node_seed_reaches_no_child() {
    let seed = 7_u8;
    let raw = [seed; 32];
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let world = world_of();
    commit(&world.work, "a/x.txt", "a2\n");
    let base = git(&world.work, &["rev-parse", "origin/main"]);
    let cmd = judgment_command(
        Path::new("/verifier"),
        &world.work,
        &base,
        None,
        Path::new("/events"),
        &LandResources {
            admit: aterm_capacity::Admit::Wait,
            timeout: None,
            test_cores: None,
        },
        &crate::EnvSnapshot::capture(),
    )
    .expect("the command");
    let mut said: Vec<String> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    said.extend(cmd.get_envs().map(|(name, value)| {
        format!(
            "{}={}",
            name.to_string_lossy(),
            value
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default()
        )
    }));
    for word in &said {
        assert!(!word.contains(&hex), "{word}");
        assert!(
            !word.contains(aterm_net::fleet_member::NODE_KEY_FILE),
            "{word}"
        );
    }

    let mac = Mac::new(&world, Some(seed));
    mac.unwritten.set(true);
    let landing = land_on(&world.work, mac.records(), judging("baba0003", &[]));
    assert_eq!(landing.code, exit::PASS, "{:#?}", landing.said);
    let pending = mac.dir().pending().expect("the spool");
    assert_eq!(mac.spooled().len(), 4);
    assert_eq!(
        pending.records.len(),
        6,
        "with the judgment's and the end's run records"
    );
    for signed in &pending.records {
        assert!(!signed.bytes.windows(raw.len()).any(|w| w == raw));
        assert!(!String::from_utf8_lossy(&signed.bytes).contains(&hex));
        let key = if signed.path.starts_with(crate::land_run::DIR) {
            let entry = crate::land_run::Entry::read(&signed.path, &signed.bytes, Origin::Spool);
            entry.body.expect("reads").signature.expect("signed").key
        } else {
            let entry = Entry::read(&signed.path, &signed.bytes, Origin::Spool);
            entry.body.expect("reads").signature.expect("signed").key
        };
        assert_eq!(key, signer(seed).key(), "the controller's signer signed it");
    }
}

/// A Mac without a node identity still lands, and writes its records unsigned. NEGATIVE
/// CONTROL: with an identity they are signed by its key. Mutant run: `record` refusing to write
/// without a signer made the unsigned arm write nothing.
#[test]
fn a_mac_without_a_node_identity_lands_and_writes_unsigned_records() {
    for seed in [None, Some(3)] {
        let world = world_of();
        commit(&world.work, "a/x.txt", "a2\n");
        let mac = Mac::new(&world, seed);
        let landing = land_on(&world.work, mac.records(), judging("baba0004", &[]));
        assert_eq!(landing.code, exit::PASS, "{:#?}", landing.said);
        let (_, tree) = &landing.judged[0];
        let entries = journal(&world.work, tree);
        assert_eq!(entries.len(), 4, "{entries:#?}");
        for entry in &entries {
            let signature = entry.body.as_ref().expect("reads").signature.as_ref();
            assert_eq!(
                signature.map(|signature| signature.key),
                seed.map(|seed| signer(seed).key()),
                "{entry:#?}"
            );
        }
    }
}

/// A PUBLISH APPENDS ONLY WHAT READS, AND FORGETS ONLY WHAT IT APPENDED. The spool holds a
/// verdict record, a file named as a record past [`crate::land_record::MAX_BYTES`] and one whose
/// bytes no reader takes; this run's own record was spooled and then appended and forgotten by
/// another landing of the clone. A refused append carried the readable record alone (neither
/// the two that do not read nor the record another landing appended), named both that do not
/// read in one line, and says every record of this run is kept (`unkept` none), since the spool
/// took each. An append during which another landing spools a newer record at a path it holds
/// leaves that newer record, and the two that do not read, in the spool. NEGATIVE CONTROLS,
/// each run once as a mutant that made this test fail: `keep` reading every put as refused (the
/// record another landing appended was appended again, and said recorded nowhere);
/// [`records::unreadable`] passing every record (the forged one was appended); the spool's
/// `done` removing by path (the newer record was gone).
#[test]
fn a_publish_appends_only_what_reads_and_forgets_only_what_it_held() {
    use std::cell::RefCell;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let dir = crate::land_spool::Dir::at(tmp.path().join("spool"));
    let me = signer(1);
    let verdict = |check, id: &str| {
        let mut made = crate::land_record::tests::record(
            check,
            Word::Ok,
            crate::land_record::tests::T,
            crate::land_record::tests::BASE,
            NOW,
        );
        made.run.id = id.into();
        made.sign(Some(&me)).unwrap()
    };
    let held = verdict(Check::Fmt, "first");
    dir.put(&held).unwrap();
    let big = verdict(Check::Lint, "big");
    let big_file = dir
        .path()
        .join(crate::land_spool::file_of(&big.path).unwrap());
    let max = usize::try_from(crate::land_record::MAX_BYTES).unwrap();
    std::fs::write(&big_file, vec![b' '; max + 1]).unwrap();
    let forged = Signed {
        bytes: b"not a record".to_vec(),
        ..verdict(Check::Test, "forged")
    };
    std::fs::write(
        dir.path()
            .join(crate::land_spool::file_of(&forged.path).unwrap()),
        &forged.bytes,
    )
    .unwrap();

    let tried: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
    let refuse = Rc::new(Cell::new(true));
    let newer = verdict(Check::Fmt, "second");
    let mut records = Records {
        append: Box::new({
            let (tried, refuse, dir, newer) =
                (tried.clone(), refuse.clone(), dir.clone(), newer.clone());
            move |_: &Path, records: &[Signed], _: u64| {
                tried
                    .borrow_mut()
                    .push(records.iter().map(|signed| signed.path.clone()).collect());
                if refuse.get() {
                    return Err("refused".to_owned());
                }
                dir.put(&newer)
            }
        }),
        spool: Box::new(dir.clone()),
        signer: None,
        host: "m45".to_owned(),
        now: Box::new(|| NOW),
        run: crate::land_run::tests::run(),
        hosting: Box::new(crate::hosting::Unhosted),
        interrupt: Box::new(|| None),
        tally: records::Tally::default(),
        lock: None,
    };
    records.run_record(
        crate::land_run::Kind::End,
        crate::land_run::tests::record(crate::land_run::Kind::End, 0),
        &mut |line| panic!("{line}"),
    );
    let mine = dir
        .pending()
        .unwrap()
        .records
        .into_iter()
        .find(|signed| signed.path.starts_with(crate::land_run::DIR))
        .expect("this run's record is spooled");
    dir.done(&mine).unwrap();

    let mut said = Vec::new();
    let refused = records.publish(tmp.path(), &mut |line| said.push(line.to_owned()));
    assert_eq!(
        refused,
        Err(records::Unwritten {
            why: "refused".to_owned(),
            unkept: None
        })
    );
    assert_eq!(*tried.borrow(), [vec![held.path.clone()]]);
    let big_named = big_file.display().to_string();
    assert_eq!(
        said.iter()
            .filter(|line| line.contains(&big_named) && line.contains(&forged.path))
            .count(),
        1,
        "{said:#?}"
    );

    refuse.set(false);
    records
        .publish(tmp.path(), &mut |_| {})
        .expect("the append holds them");
    assert_eq!(tried.borrow().last(), Some(&vec![held.path.clone()]));
    let pending = dir.pending().unwrap();
    assert_eq!(pending.records, [newer, forged]);
    let unread: Vec<&PathBuf> = pending.unreadable.iter().map(|(file, _)| file).collect();
    assert_eq!(unread, [&big_file]);
}
