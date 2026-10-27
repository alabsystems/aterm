// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-learn`'s file I/O, arguments and summary. The verbs' sessions are m6's (its unit tests
//! run them over memory); these hold the lab's half: the files behind `DurableLog` and
//! `DurableStore`, which no other test reaches.

use std::fs;

use aterm_spec::wait::MUST_HAPPEN;

use super::*;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// A usage or refusal's exit code; 0 for a parse that succeeds.
fn code(a: &[&str]) -> i32 {
    parse(&args(a)).err().map_or(0, |f| f.code)
}

const BUILD: &str = "aterm-lab 0.1.0+g0123456789ab";

fn hex(b: u8) -> String {
    Digest([b; 32]).hex()
}

#[test]
fn each_verb_takes_its_flags_alone() {
    assert_eq!(
        parse(&args(&["init", "--store", "s", "--practice", "0"])).ok(),
        Some(Verb::Init {
            store: "s".to_owned(),
            pair: Pair::Practice(0)
        })
    );
    assert_eq!(
        parse(&args(&["chain"])).ok(),
        Some(Verb::Chain { threads: 1 })
    );
    assert_eq!(parse(&args(&["build"])).ok(), Some(Verb::Build));
    assert_eq!(
        parse(&args(&["verify", "--threads", "8", "--store", "s"])).ok(),
        Some(Verb::Verify {
            store: "s".to_owned(),
            threads: 8
        })
    );
    assert_eq!(
        parse(&args(&["rollback", "--store", "s", "--to", &hex(0xab)])).ok(),
        Some(Verb::Rollback {
            store: "s".to_owned(),
            to: Digest([0xab; 32])
        })
    );
    let d = hex(7);
    assert_eq!(
        parse(&args(&[
            "session",
            "--store",
            "b",
            "--practice",
            "2",
            "--tests-log-digest",
            &d,
            "--replay",
            "a",
        ]))
        .ok(),
        Some(Verb::Session(SessionArgs {
            store: "b".to_owned(),
            pair: Pair::Practice(2),
            tests_log: Digest([7; 32]),
            threads: 1,
            replay: Some("a".to_owned()),
        }))
    );
    let upper = hex(0xab).to_uppercase();
    for refused in [
        &[][..],
        &["frobnicate"],
        &["chain", "--store", "s"],
        &["chain", "--to", &d],
        &["build", "--threads", "2"],
        &["build", "extra"],
        &["init", "--store", "s"],
        &["init", "--practice", "0"],
        &["init", "--store", "s", "--practice", "0", "--threads", "2"],
        &["init", "--store", "s", "--practice", "01"],
        &["init", "--store", "s", "--practice", "-1"],
        &["init", "--store", "s", "--practice"],
        &["train"],
        &["train", "--store", "s", "--threads", "0"],
        &["train", "--store", "s", "--threads", "65"],
        &["train", "--store", "s", "--threads", "08"],
        &["train", "--store", "s", "--replay", "a"],
        &["verify", "--store", "s", "extra"],
        &["rollback", "--store", "s"],
        &["rollback", "--store", "s", "--to", &upper],
        &["rollback", "--store", "s", "--to", &d[1..]],
        &["session", "--store", "s", "--practice", "0"],
        &["session", "--store", "s", "--tests-log-digest", &d],
        &[
            "session",
            "--store",
            "b",
            "--practice",
            "0",
            "--tests-log-digest",
            &d,
            "--replay",
            "a",
            "--threads",
            "2",
        ],
    ] {
        assert_eq!(code(refused), 2, "{refused:?}");
    }
}

#[test]
fn a_digest_is_sixty_four_lowercase_hex_digits() {
    for b in [0, 1, 0x5a, 0xff] {
        assert_eq!(parse_digest(&hex(b)), Some(Digest([b; 32])));
    }
    let good = hex(0xab);
    for bad in [
        String::new(),
        good[1..].to_owned(),
        format!("{good}0"),
        good.to_uppercase(),
        format!("g{}", &good[1..]),
        format!("+{}", &good[1..]),
    ] {
        assert_eq!(parse_digest(&bad), None, "{bad}");
    }
}

#[test]
fn a_ledger_directory_names_its_pair_canonically() {
    assert_eq!(pair_named("pair-1"), Some(Pair::Real(1)));
    assert_eq!(pair_named("practice-0"), Some(Pair::Practice(0)));
    assert_eq!(pair_named("practice-12"), Some(Pair::Practice(12)));
    for bad in [
        "pair-",
        "pair-01",
        "pair-x",
        "pair-+1",
        "practice-00",
        "pair1",
        "Pair-1",
    ] {
        assert_eq!(pair_named(bad), None, "{bad}");
    }
    for pair in [Pair::Real(3), Pair::Practice(0), Pair::Practice(40)] {
        assert_eq!(pair_named(&pair.name()), Some(pair));
    }
}

#[test]
fn a_ledgers_lines_are_made_once_and_read_whole() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let root = tmp.path().join("root");
    let pair = Pair::Practice(0);
    let mut log = LabLog::open(&root, pair).expect("the writer");
    assert!(
        LabLog::open(&root, Pair::Practice(1)).is_err(),
        "one writer per version root"
    );
    assert_eq!(log.lines(), Ok(Vec::new()));
    log.append(0, b"zero").expect("line 0");
    assert_eq!(log.append(0, b"again"), Err(LogFault::Exists));
    log.append(1, b"one").expect("line 1");
    assert_eq!(log.lines(), Ok(vec![b"zero".to_vec(), b"one".to_vec()]));
    let mut ro = LabLog::read_only(&root, pair);
    assert_eq!(ro.lines(), log.lines());
    assert!(matches!(ro.append(2, b"two"), Err(LogFault::Io(_))));
    assert!(
        fs::read_dir(root.join(".tmp"))
            .expect("the temps")
            .next()
            .is_none(),
        "no temp file is left"
    );
    let dir = root.join("practice-0");
    for stray in ["3.line", "02.line", "x.line", ".line", "notes"] {
        fs::write(dir.join(stray), b"?").expect("planted");
        assert!(matches!(log.lines(), Err(LogFault::Io(_))), "{stray}");
        fs::remove_file(dir.join(stray)).expect("removed");
    }
    assert_eq!(log.lines().map(|l| l.len()), Ok(2));
    fs::write(dir.join("2.line"), vec![b'x'; 4 * 1024 + 1]).expect("planted");
    assert!(
        matches!(log.lines(), Err(LogFault::Io(_))),
        "a line past its cap"
    );
    drop(log);
    LabLog::open(&root, pair).expect("the lock goes with the log");
}

#[cfg(debug_assertions)]
#[test]
fn every_new_name_is_flushed_durably_in_its_parent() {
    use aterm_durable::Flush;
    let rec = aterm_durable::seam::Recording::start();
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let deep = tmp.path().join("a/b/c");
    make_dirs(&deep).expect("made");
    assert_eq!(
        rec.take(),
        [Flush::Durable; 3],
        "one flush per new directory"
    );
    make_dirs(&deep).expect("made again");
    assert_eq!(rec.take(), [], "a directory that exists was made durable");
    let root = tmp.path().join("root");
    let mut log = LabLog::open(&root, Pair::Practice(0)).expect("the writer");
    assert_eq!(
        rec.take(),
        [Flush::Durable; 2],
        "the root in its parent, the pair's directory in the root"
    );
    log.append(0, b"zero").expect("line 0");
    assert_eq!(
        rec.take(),
        [Flush::Durable; 2],
        "the line's temp, then its name in the pair's directory"
    );
    let dir = tmp.path().join("s");
    fs::create_dir(&dir).expect("its directory");
    let mut io = LabStore::open(dir).expect("the store");
    let _ = rec.take();
    io.create_dir(
        Step::VersionFiles,
        "versions/aa",
        &[("state", b"s"), ("manifest", b"m")],
    )
    .expect("made whole");
    assert_eq!(
        rec.take(),
        [Flush::Durable; 5],
        "versions/ in the store, two files, the whole directory, its name in versions/"
    );
    io.replace(Step::Pointer, "active", b"p").expect("made");
    assert_eq!(rec.take(), [Flush::Durable; 2], "the temp, then its name");
    drop(new_store("session", &tmp.path().join("fresh")).expect("a new store"));
    assert_eq!(
        rec.take(),
        [Flush::Durable],
        "a new store's directory in its parent"
    );
    let runner_root = tmp.path().join("v/root");
    let runner = LabLog::hold_runner(&runner_root).expect("the runner");
    assert_eq!(
        rec.take(),
        [Flush::Durable; 2],
        "the version root, made by the runner's lock"
    );
    assert!(LabLog::hold_runner(&runner_root).is_err(), "one runner");
    drop(runner);
    LabLog::hold_runner(&runner_root).expect("the lock goes with the runner");
}

/// Each flush is of the parent and after the name is made: a directory that can be written and
/// searched but not opened refuses the flush of a name made in it, after the name exists.
#[test]
fn a_new_name_is_flushed_in_its_parent_after_it_is_made() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let shut = |dir: &Path, mode: u32| {
        fs::set_permissions(dir, fs::Permissions::from_mode(mode)).expect("its mode");
    };
    let p = tmp.path().join("p");
    fs::create_dir(&p).expect("a directory");
    shut(&p, 0o300);
    let made = make_dirs(&p.join("x"));
    shut(&p, 0o700);
    assert!(made.is_err(), "the new name is flushed in its parent");
    assert!(p.join("x").is_dir(), "and only after it was made");
    let root = tmp.path().join("root");
    let mut log = LabLog::open(&root, Pair::Practice(0)).expect("the writer");
    log.append(0, b"zero").expect("line 0");
    let dir = root.join("practice-0");
    shut(&dir, 0o300);
    let appended = log.append(1, b"one");
    shut(&dir, 0o700);
    assert!(
        matches!(appended, Err(LogFault::Io(_))),
        "the line's name is flushed in its directory"
    );
    assert!(dir.join("1.line").exists(), "after it was linked");
}

#[test]
fn a_session_runs_only_under_its_own_live_runner_and_the_runner_waits_for_the_ledger() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let root = tmp.path().join("root");
    let me = std::process::id();
    assert!(
        LabLog::need_runner_of(&root, me).is_err(),
        "no runner has run"
    );
    make_dirs(&root).expect("the root");
    fs::write(root.join("runner.lock"), "4000000000\n").expect("an earlier runner's longer pid");
    let runner = LabLog::hold_runner(&root).expect("the runner");
    assert_eq!(
        fs::read_to_string(root.join("runner.lock")).expect("the lock file"),
        format!("{me}\n"),
        "the runner's pid alone"
    );
    LabLog::need_runner_of(&root, me).expect("a child of the live runner");
    let other = LabLog::need_runner_of(&root, me + 1).expect_err("another runner's");
    assert!(
        other.contains("not this session's") && other.contains(&format!("pid {me}")),
        "{other}"
    );
    assert!(
        LabLog::need_runner(&root).is_err(),
        "this test's own parent is not the runner"
    );
    drop(runner);
    assert!(
        LabLog::need_runner_of(&root, me).is_err(),
        "a runner that has gone, and the check takes nothing"
    );
    let runner = LabLog::hold_runner(&root).expect("the check left the lock free");
    fs::write(root.join("runner.lock"), "").expect("a pid not yet written");
    assert!(
        LabLog::need_runner_of(&root, me)
            .is_err_and(|e| e.contains("not this session's") && e.contains("no pid")),
        "a holder that names no pid admits no session"
    );
    drop(runner);
    LabLog::hold_runner(&root).expect("the runner again");
    let pause = |d: Duration| {
        std::thread::sleep(d);
        Ok(())
    };
    let child = LabLog::open(&root, Pair::Practice(0)).expect("a child's ledger");
    assert!(
        LabLog::open_within(&root, Pair::Practice(0), Duration::ZERO, &pause).is_err(),
        "held"
    );
    let pauses = std::cell::Cell::new(0);
    let stopped = LabLog::open_within(&root, Pair::Practice(0), MUST_HAPPEN, &|_| {
        pauses.set(pauses.get() + 1);
        Err("stopped".to_owned())
    });
    assert_eq!(stopped.err(), Some("stopped".to_owned()));
    assert_eq!(pauses.get(), 1, "the pause's error ends the wait");
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_500));
        drop(child);
    });
    let taken = LabLog::open_within(&root, Pair::Practice(0), MUST_HAPPEN, &pause)
        .expect("taken once the child lets it go");
    release.join().expect("the releasing thread");
    drop(taken);
    LabLog::open_within(&root, Pair::Practice(0), MUST_HAPPEN, &|_| {
        Err("paused".to_owned())
    })
    .expect("a free lock is taken with no pause");
}

#[test]
fn the_version_root_lists_every_pair_and_refuses_a_stray_name() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let root = tmp.path().join("root");
    // Made in an order neither sorted nor reversed, and with practice-10 before practice-2 by
    // name: the root lists them in the pairs' own order.
    let made = [
        Pair::Practice(2),
        Pair::Real(3),
        Pair::Practice(10),
        Pair::Real(1),
        Pair::Practice(0),
    ];
    for pair in made {
        let mut log = LabLog::open(&root, pair).expect("its writer");
        log.append(0, pair.name().as_bytes()).expect("its line");
    }
    let r1 = LabLog::open(&root, Pair::Real(1)).expect("pair 1");
    fs::create_dir(root.join("notes")).expect("a directory no pair names");
    let listed: Vec<(Pair, Vec<Vec<u8>>)> = [
        Pair::Real(1),
        Pair::Real(3),
        Pair::Practice(0),
        Pair::Practice(2),
        Pair::Practice(10),
    ]
    .iter()
    .map(|p| (*p, vec![p.name().into_bytes()]))
    .collect();
    assert_eq!(r1.root(), Ok(listed));
    fs::create_dir(root.join("pair-01")).expect("planted");
    assert!(matches!(r1.root(), Err(LogFault::Io(_))));
    let entries = LabLog::pairs(&root).expect("the root");
    assert_eq!(entries.len(), 6);
    assert_eq!(entries[0].0, "pair-01", "a name that names no pair, first");
    assert!(matches!(entries[0].1, Err(LogFault::Io(_))));
    assert!(
        entries[1..].iter().all(|(_, read)| read.is_ok()),
        "one directory's fault is its own"
    );
    assert_eq!(entries[1].0, "pair-1", "then the pairs in their own order");
    assert_eq!(
        LabLog::read_only(&tmp.path().join("none"), Pair::Real(1)).root(),
        Ok(Vec::new())
    );
}

#[test]
fn the_chain_file_is_made_once_by_a_writer() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let root = tmp.path().join("root");
    let pair = Pair::Practice(0);
    let first = Digest([7; 32]);
    assert!(
        matches!(
            LabLog::read_only(&root, pair).chain(&first),
            Err(LogFault::Io(_))
        ),
        "a read-only log makes no chain file"
    );
    let mut log = LabLog::open(&root, pair).expect("the writer");
    assert_eq!(log.chain(&first), Ok(first));
    assert_eq!(log.chain(&Digest([8; 32])), Ok(first), "the first stays");
    assert_eq!(
        LabLog::read_only(&root, pair).chain(&Digest([9; 32])),
        Ok(first)
    );
    fs::write(root.join("chain"), "not a digest\n").expect("planted");
    assert!(matches!(log.chain(&first), Err(LogFault::Io(_))));
}

/// A fresh store for practice pair 0 in a new directory under `tmp`.
fn store_in(tmp: &Path, name: &str, nonce: u64) -> (PathBuf, Store<LabStore>) {
    let dir = tmp.join(name);
    fs::create_dir(&dir).expect("its directory");
    let io = LabStore::open(dir.clone()).expect("the store");
    let store = Store::init(io, BUILD, Pair::Practice(0), nonce, Digest([0; 32])).expect("v0");
    (dir, store)
}

#[test]
fn a_practice_ledger_runs_over_the_files() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let (_, store) = store_in(tmp.path(), "s", 1);
    let v0 = store
        .version(&store.pointer().active)
        .expect("v0 is kept")
        .clone();
    let pin = ChainPin::of(&vec![v0; 5]).expect("five members");
    let root = tmp.path().join("root");
    let pair = Pair::Practice(0);
    let mut ledger = Ledger::practice(LabLog::open(&root, pair).expect("the writer"), 0, pin)
        .expect("an empty ledger");
    ledger
        .open(store.id(), BUILD, Digest([2; 32]), false)
        .expect("the open line");
    let raw = LabLog::read_only(&root, pair).lines().expect("its lines");
    assert_eq!(raw.len(), 1);
    assert_eq!(ledger_head(pair, &raw), Ok(ledger.head()));
    assert!(
        ledger_head(Pair::Practice(1), &raw).is_err(),
        "another pair's genesis"
    );
    assert!(
        LabLog::open(&root, pair).is_err(),
        "the ledger holds the root's lock while it lives"
    );
    drop(ledger);
    let again =
        Ledger::practice(LabLog::open(&root, pair).expect("the writer"), 0, pin).expect("reloaded");
    assert_eq!(again.lines().len(), 1);
    assert_eq!(again.head(), ledger_head(pair, &raw).expect("its head"));
}

#[test]
fn a_store_directory_holds_only_its_own_names() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path().join("s");
    assert!(LabStore::open(dir.clone()).is_err(), "no directory");
    fs::create_dir(&dir).expect("its directory");
    let mut io = LabStore::open(dir.clone()).expect("the store");
    assert!(LabStore::open(dir.clone()).is_err(), "one holder");
    for bad in [
        "", "../x", "a/../b", ".tmp/x", "a//b", "/abs", ".lock", "a/", "./a",
    ] {
        assert!(matches!(io.read(bad), Err(StoreFault::Io(_))), "{bad:?}");
        assert!(
            matches!(io.create(Step::Eval, bad, b"x"), Err(StoreFault::Io(_))),
            "{bad:?}"
        );
        assert!(
            matches!(io.replace(Step::Pointer, bad, b"x"), Err(StoreFault::Io(_))),
            "{bad:?}"
        );
    }
    assert_eq!(io.read("eval/1.rec"), Ok(None));
    assert_eq!(io.list("eval"), Ok(Vec::new()));
    io.create(Step::Eval, "eval/1.rec", b"one")
        .expect("created");
    assert_eq!(
        io.create(Step::Eval, "eval/1.rec", b"two"),
        Err(StoreFault::Exists("eval/1.rec".to_owned()))
    );
    assert_eq!(
        io.read("eval/1.rec"),
        Ok(Some(b"one".to_vec())),
        "a refused create leaves the file"
    );
    io.create_dir(
        Step::VersionFiles,
        "versions/aa",
        &[("state", b"s"), ("manifest", b"m")],
    )
    .expect("made whole");
    assert_eq!(
        io.create_dir(Step::VersionFiles, "versions/aa", &[("state", b"t")]),
        Err(StoreFault::Exists("versions/aa".to_owned()))
    );
    let escape = tmp.path().join("escape");
    let absolute = escape.to_string_lossy().into_owned();
    for bad in ["../state", "a/b", ".hidden", "", &absolute] {
        assert!(
            matches!(
                io.create_dir(
                    Step::VersionFiles,
                    "versions/bb",
                    &[("state", b"s"), (bad, b"t")]
                ),
                Err(StoreFault::Io(_))
            ),
            "{bad:?}"
        );
    }
    assert!(
        !escape.exists(),
        "a version file never lands outside the store"
    );
    assert_eq!(
        io.list("versions"),
        Ok(vec!["aa".to_owned()]),
        "a refused directory is not made in part"
    );
    assert_eq!(
        io.list("versions/aa"),
        Ok(vec!["manifest".to_owned(), "state".to_owned()])
    );
    io.replace(Step::Pointer, "active", b"p1").expect("made");
    io.replace(Step::Pointer, "active", b"p2")
        .expect("replaced");
    assert_eq!(io.read("active"), Ok(Some(b"p2".to_vec())));
    fs::write(dir.join("big"), vec![0; 64 * 1024 + 1]).expect("planted");
    assert!(
        matches!(io.read("big"), Err(StoreFault::Io(_))),
        "past the cap"
    );
    fs::remove_file(dir.join("big")).expect("removed");
    fs::write(dir.join("eval/.hidden"), b"xxxx").expect("planted");
    fs::write(dir.join(".tmp/stale"), b"yyyyy").expect("planted");
    io.create(Step::Revision, "revisions/0000", b"rrrrrrr")
        .expect("a revision");
    io.create(Step::StoreFile, "store.txt", b"ssssssssssss")
        .expect("the store file");
    io.create(Step::Report, "report.txt", b"tttttttttttttt")
        .expect("the report");
    assert_eq!(
        io.retained().expect("sized"),
        (3 + 2 + 7, 1),
        "versions/, revisions/ and eval/ alone, and no dot-named file: not store.txt, the \
         pointer, the report or a temp"
    );
    fs::remove_file(dir.join("eval/.hidden")).expect("removed");
    fs::remove_file(dir.join(".tmp/stale")).expect("removed");
    assert!(
        fs::read_dir(dir.join(".tmp"))
            .expect("the temps")
            .next()
            .is_none(),
        "no temp file is left"
    );
    drop(io);
    LabStore::open(dir).expect("the lock goes with the store");
}

#[test]
fn the_seam_stops_after_the_nth_write_of_a_step_once_it_is_durable() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path().join("s");
    fs::create_dir(&dir).expect("its directory");
    let mut io = LabStore::open(dir)
        .expect("the store")
        .stopping(Step::Eval, 2);
    io.create(Step::Eval, "eval/1.rec", b"1")
        .expect("the first");
    io.create(Step::Figures, "eval/1.figures", b"f")
        .expect("another step");
    assert_eq!(
        io.create(Step::Eval, "eval/2.rec", b"2"),
        Err(StoreFault::Stopped(Step::Eval))
    );
    assert_eq!(
        io.read("eval/2.rec"),
        Ok(Some(b"2".to_vec())),
        "the stopped write is durable"
    );
    io.create(Step::Eval, "eval/3.rec", b"3")
        .expect("the third");
}

#[test]
fn a_store_round_trips_over_the_files_and_a_torn_one_is_refused() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let (dir, store) = store_in(tmp.path(), "s", 1);
    let (id, pointer) = (store.id(), store.pointer());
    let v0 = pointer.active;
    drop(store);
    let mut loaded =
        Store::load(LabStore::open(dir.clone()).expect("the store"), BUILD).expect("loads");
    assert_eq!((loaded.id(), loaded.pointer()), (id, pointer));
    let moved = loaded
        .rollback(v0, Digest([0; 32]))
        .expect("a rollback to v0");
    assert_eq!(
        moved
            .written
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        ["revisions/0001", "active"]
    );
    let after = loaded.pointer();
    drop(loaded);
    assert_eq!(
        Store::load(LabStore::open(dir.clone()).expect("the store"), BUILD)
            .expect("loads")
            .pointer(),
        after
    );
    assert!(
        Store::load(
            LabStore::open(dir.clone()).expect("the store"),
            "another-build"
        )
        .is_err(),
        "another build's store"
    );

    let (torn, store) = store_in(tmp.path(), "torn", 2);
    drop(store);
    let mut s = Store::load(
        LabStore::open(torn.clone())
            .expect("the store")
            .stopping(Step::Revision, 1),
        BUILD,
    )
    .expect("loads");
    assert!(format!("{:?}", s.rollback(v0, Digest([0; 32]))).contains("Stopped(Revision)"));
    drop(s);
    assert!(
        format!(
            "{:?}",
            Store::load(LabStore::open(torn).expect("the store"), BUILD).err()
        )
        .contains("StoreTorn")
    );

    let (whole, store) = store_in(tmp.path(), "whole", 3);
    drop(store);
    let mut s = Store::load(
        LabStore::open(whole.clone())
            .expect("the store")
            .stopping(Step::Pointer, 1),
        BUILD,
    )
    .expect("loads");
    assert!(format!("{:?}", s.rollback(v0, Digest([0; 32]))).contains("Stopped(Pointer)"));
    drop(s);
    assert_eq!(
        Store::load(LabStore::open(whole).expect("the store"), BUILD)
            .expect("a stop after the pointer is the new state")
            .pointer()
            .revision,
        1
    );
}

#[test]
fn a_summary_round_trips_and_refuses_anything_else() {
    let full = Summary {
        build: BUILD.to_owned(),
        profile: "debug".to_owned(),
        child: "A".to_owned(),
        pair: "practice-0".to_owned(),
        store: Some(Digest([1; 32])),
        result: "ok".to_owned(),
        bits: "10011".to_owned(),
        mf: Some(Digest([2; 32])),
        r2: Some(1),
        report: Some(Digest([3; 32])),
        criteria: vec!["C1 Mf-U wins=7 losses=1 gain=10/2 against=3/1 shown".to_owned()],
        moves: vec![
            "train written=100 read=0".to_owned(),
            "rollback written=60 read=4000".to_owned(),
        ],
        retained: Some((12_345, 5)),
        wall_ms: 99,
        held_out_judged: 0,
        footprint: Some(1 << 20),
    };
    let bare = Summary {
        child: "B".to_owned(),
        ..Summary::default()
    };
    for s in [&full, &bare] {
        assert_eq!(Summary::parse(&s.text()).as_ref(), Some(s));
    }
    let text = full.text();
    let lines: Vec<&str> = text.lines().collect();
    let without = |n: usize| {
        let mut l = lines.clone();
        l.remove(n);
        l.join("\n")
    };
    let swapped = {
        let mut l = lines.clone();
        l.swap(1, 2);
        l.join("\n")
    };
    for bad in [
        String::new(),
        text.replace(SUMMARY, "aos-learn-session-v2"),
        without(0),
        without(4),
        without(lines.len() - 1),
        format!("{text}extra\n"),
        swapped,
        text.replace("bits 10011", "bits 10021"),
        text.replace("wall_ms 99 MEASURED", "wall_ms 99"),
        text.replace(&Digest([2; 32]).hex(), "nothex"),
    ] {
        assert_eq!(Summary::parse(&bad), None, "{bad}");
    }
}

#[test]
fn the_cost_report_names_each_file_and_labels_its_time() {
    assert_eq!(
        cost_report(
            "rollback",
            &[
                ("revisions/0002".to_owned(), 140),
                ("active".to_owned(), 72)
            ],
            &[("versions/x/state".to_owned(), 4_000)],
            9_000,
            3,
            5,
        ),
        "aos-learn rollback: wrote 212 bytes (revisions/0002 140, active 72); read 4000 bytes to \
         validate (1 files); retained 9000 bytes in 3 versions; 5 ms MEASURED\n"
    );
}

#[test]
fn the_pin_source_is_the_constant_stage_e_commits() {
    let pin = ChainPin {
        ids: [1, 2, 3, 4, 5].map(|b| Digest([b; 32])),
        labels: [11, 12, 13, 14, 15].map(|b| Digest([b; 32])),
    };
    let src = pin_source(&pin);
    assert!(src.contains("pub const M6_CHAIN_V1: Option<ChainPin> = Some(ChainPin {"));
    assert!(src.contains(&pin.digest().hex()));
    assert_eq!(src.matches("        Digest([0x").count(), 10);
    let (ids, labels) = src
        .split_once("    labels: [")
        .expect("the labels follow the ids");
    let at = |part: &str, d: &Digest| part.find(&byte_array(d));
    let mut last = (0, 0);
    for k in 0..5 {
        let (id, label) = (&pin.ids[k], &pin.labels[k]);
        let (Some(i), Some(l)) = (at(ids, id), at(labels, label)) else {
            panic!("member {k} out of place:\n{src}");
        };
        assert!(
            at(labels, id).is_none() && at(ids, label).is_none(),
            "{src}"
        );
        assert!(
            k == 0 || (i > last.0 && l > last.1),
            "member {k} out of order:\n{src}"
        );
        last = (i, l);
    }
}

#[test]
fn every_exit_is_checked_against_held_out_judged() {
    assert_eq!(guarded("aos-learn", Ok(0), 0).ok(), Some(0));
    assert_eq!(
        guarded("aos-learn", Err(Fail::usage("u")), 0)
            .err()
            .map(|f| f.code),
        Some(2)
    );
    for out in [Ok(0), Err(Fail::usage("u")), Err(Fail::die("d"))] {
        let f = guarded("aos-learn", out, 1).expect_err("a defect");
        assert_eq!(f.code, 1);
        assert_eq!(
            f.msg.as_deref(),
            Some("aos-learn: defect: held_out_judged reads 1, not 0")
        );
    }
}

#[test]
fn a_verb_exits_by_its_named_codes() {
    assert_eq!(cmd_aos_learn(&args(&["--help"])).ok(), Some(0));
    let exit = |a: &[&str]| parse(&args(a)).and_then(|v| run(&v)).err();
    assert_eq!(exit(&["chain", "--store", "s"]).map(|f| f.code), Some(2));
    let homes = || HOMES.with(std::cell::Cell::get);
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let full = tmp.path().join("full");
    fs::create_dir(&full).expect("a directory");
    fs::write(full.join("x"), b"x").expect("not empty");
    let given = full.to_string_lossy().into_owned();
    let d = hex(1);
    let before = homes();
    for verb in [
        &["init", "--store", &given, "--practice", "0"][..],
        &[
            "session",
            "--store",
            &given,
            "--practice",
            "0",
            "--tests-log-digest",
            &d,
        ],
    ] {
        assert_eq!(
            exit(verb).map(|f| f.code),
            Some(2),
            "{verb:?}: a store is made new"
        );
    }
    assert_eq!(
        homes(),
        before,
        "a store that is not new is refused before any home is read"
    );
    assert_eq!(
        fs::read_dir(&full).expect("read").count(),
        1,
        "a refused verb leaves the directory"
    );
    let empty = tmp.path().join("empty");
    fs::create_dir(&empty).expect("a directory");
    for store in [&empty, &tmp.path().join("missing")] {
        let f =
            exit(&["rollback", "--store", &store.to_string_lossy(), "--to", &d]).expect("refused");
        assert_eq!(f.code, 1, "{:?}", f.msg);
        assert!(
            f.msg
                .as_deref()
                .is_some_and(|m| m.starts_with("aos-learn rollback: refused Store: ")),
            "{:?}",
            f.msg
        );
    }
    assert_eq!(
        homes(),
        before,
        "a store that does not load is refused first"
    );
    assert_eq!(
        fs::read_dir(&empty).expect("read").count(),
        0,
        "and nothing is made in it"
    );
}
