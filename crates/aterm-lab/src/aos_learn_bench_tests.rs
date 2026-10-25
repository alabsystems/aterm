// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The runner's gates, store comparison and record, over synthetic children and real store
//! directories. A whole session needs `M6_CHAIN_V1` (stage E) and the account's own home, so
//! no test here spawns one; the rehearsal does (design §13.9).

use std::fs;

use super::*;

const BUILD: &str = "aterm-lab 0.1.0+g0123456789ab";

fn summary(child: &str, store: u8) -> Summary {
    Summary {
        build: BUILD.to_owned(),
        profile: "debug".to_owned(),
        child: child.to_owned(),
        pair: "practice-0".to_owned(),
        store: Some(Digest([store; 32])),
        result: "ok".to_owned(),
        bits: "10110".to_owned(),
        mf: Some(Digest([9; 32])),
        r2: Some(2),
        report: (child == "A").then_some(Digest([8; 32])),
        criteria: Vec::new(),
        moves: Vec::new(),
        retained: Some((1_000, 5)),
        wall_ms: 10,
        held_out_judged: 0,
        footprint: Some(100 << 20),
    }
}

fn child(name: &str, store: u8) -> Child {
    Child {
        name: name.to_owned(),
        spawned: true,
        exit: Some(0),
        summary: Some(summary(name, store)),
        ..Child::default()
    }
}

fn open(store: u8, tests_log: u8, replay: bool) -> Line {
    Line::Open {
        store: Digest([store; 32]),
        build: BUILD.to_owned(),
        chain: Digest([3; 32]),
        tests_log: Digest([tests_log; 32]),
        replay,
    }
}

/// A finished session's inputs: A on store 1, B on store 2, bits 10110 recorded.
fn finished() -> Inputs {
    let mut session = vec![open(1, 4, false)];
    for bit in [true, false, true, true, false] {
        session.push(Line::LookVerdict { bit });
    }
    session.push(Line::Retire);
    session.push(open(2, 4, true));
    Inputs {
        pair: Pair::Practice(0),
        build: BUILD.to_owned(),
        profile: "debug".to_owned(),
        lab: (Digest([5; 32]), Some(Digest([5; 32]))),
        tests_log: Digest([4; 32]),
        children: [child("A", 1), child("B", 2)],
        session: Some(session),
        stopped: false,
        stores: Some(Vec::new()),
    }
}

/// The session's lines in `i`, which [`finished`] read.
fn lines(i: &mut Inputs) -> &mut Vec<Line> {
    i.session.as_mut().expect("read")
}

/// The gates that missed, each once (a miss may name both children).
fn gates_of(i: &Inputs) -> Vec<&'static str> {
    let mut gates: Vec<&'static str> = decide(i).failed.iter().map(|(g, _)| *g).collect();
    gates.dedup();
    gates
}

#[test]
fn a_finished_session_with_every_gate_passed_is_recorded() {
    let g = decide(&finished());
    assert_eq!(g.failed, Vec::new());
    assert_eq!(g.line, Some(Verdict::Record));
}

#[test]
fn a_session_stopped_before_its_open_gets_no_line() {
    let mut i = finished();
    lines(&mut i).clear();
    i.children[0].exit = Some(1);
    i.children[0].summary.as_mut().expect("A's").result = "refused NotReproduced".to_owned();
    i.children[1] = Child {
        name: "B".to_owned(),
        ..Child::default()
    };
    i.stores = None;
    assert_eq!(decide(&i).line, None);
    i.session = Some(vec![open(2, 4, true)]);
    assert_eq!(
        decide(&i).line,
        None,
        "lines that hold no session's own open"
    );
}

#[test]
fn each_childs_end_names_its_void_cause() {
    let b_unrun = || Child {
        name: "B".to_owned(),
        ..Child::default()
    };
    type End = fn(&mut Child);
    let cases: [(&str, End, VoidCause); 6] = [
        (
            "deadline",
            |c| {
                c.deadline = true;
                c.signal = Some(9);
                c.exit = None;
            },
            VoidCause::Deadline,
        ),
        ("exit 1", |c| c.exit = Some(1), VoidCause::ChildFailed),
        (
            "exit 0 with no summary",
            |c| c.summary = None,
            VoidCause::ChildFailed,
        ),
        (
            "refused",
            |c| c.summary.as_mut().expect("its").result = "refused".to_owned(),
            VoidCause::ChildFailed,
        ),
        (
            "a signal",
            |c| {
                c.signal = Some(15);
                c.exit = None;
            },
            VoidCause::Other,
        ),
        (
            "an interrupt",
            |c| {
                c.interrupted = true;
                c.signal = Some(9);
                c.exit = None;
            },
            VoidCause::Other,
        ),
    ];
    for (what, end, cause) in cases {
        let mut i = finished();
        end(&mut i.children[0]);
        i.children[1] = b_unrun();
        i.stores = None;
        assert_eq!(decide(&i).line, Some(Verdict::Void(cause)), "A: {what}");
        if what == "exit 0 with no summary" {
            assert_eq!(
                decide(&i).failed,
                Vec::new(),
                "an A with no summary: its guard, open line and footprint are not read"
            );
            let text = record_text(
                &i,
                &decide(&i),
                &Wrote::Void(cause, VoidClass::Stopped),
                &[],
            );
            assert!(
                text.contains("gates: G3's guard not read for child A, which left no summary\n")
                    && !text.contains("G3 FAILED"),
                "{text}"
            );
        }
        if matches!(what, "exit 1" | "refused") {
            let s = i.children[0].summary.as_mut().expect("A's");
            s.footprint = Some(MAX_FOOTPRINT + 1);
            lines(&mut i)[0] = open(1, 6, false);
            assert_eq!(
                decide(&i).failed,
                Vec::new(),
                "A: {what}: an unfinished A's open line and footprint are not read, though it \
                 left a summary"
            );
        }
        let mut i = finished();
        end(&mut i.children[1]);
        i.stores = None;
        assert_eq!(decide(&i).line, Some(Verdict::Void(cause)), "B: {what}");
        if what == "exit 0 with no summary" {
            assert_eq!(
                decide(&i).failed,
                Vec::new(),
                "a B with no summary: its guard, open line and footprint are not read"
            );
        }
        if matches!(what, "exit 1" | "refused") {
            let s = i.children[1].summary.as_mut().expect("B's");
            s.footprint = Some(MAX_FOOTPRINT + 1);
            lines(&mut i)[7] = open(2, 6, true);
            assert_eq!(
                decide(&i).failed,
                Vec::new(),
                "B: {what}: an unfinished B's open line and footprint are not read, though it \
                 left a summary"
            );
        }
    }
    let mut i = finished();
    i.children[1] = Child {
        name: "B".to_owned(),
        error: Some("cannot spawn".to_owned()),
        ..Child::default()
    };
    i.stores = None;
    assert_eq!(
        decide(&i).line,
        Some(Verdict::Void(VoidCause::Other)),
        "B never ran after A finished"
    );
    let mut i = finished();
    i.children[0].deadline = true;
    assert!(gates_of(&i).contains(&"G4"), "a deadline kill is G4's miss");
    let mut i = finished();
    i.children[0] = Child {
        name: "A".to_owned(),
        spawned: true,
        signal: Some(9),
        deadline: true,
        ..Child::default()
    };
    i.children[1] = b_unrun();
    lines(&mut i).truncate(3);
    i.stores = None;
    assert_eq!(
        decide(&i).line,
        Some(Verdict::Void(VoidCause::Deadline)),
        "an A killed before it printed its summary"
    );
    let g = decide(&i);
    assert_eq!(
        g.failed,
        [("G4", "child A was killed at the deadline".to_owned())]
    );
    let text = record_text(
        &i,
        &g,
        &Wrote::Void(VoidCause::Deadline, VoidClass::Stopped),
        &[],
    );
    assert!(
        text.contains(
            "gates: G2 not decided, nor G3's open line and G4's footprint for a child that did \
             not finish\n"
        ) && text.contains("gates: G3's guard not read for child B, which did not run\n")
            && text.contains("gates: G3's guard not read for child A, which left no summary\n")
            && !text.contains("child A, which did not run")
            && !text.contains("child B, which left no summary")
            && !text.contains("an interrupt came"),
        "{text}"
    );
}

#[test]
fn the_runner_writes_only_for_its_own_sessions_lines() {
    let (a, other) = (Digest([1; 32]), Digest([2; 32]));
    let mut session = finished().session.expect("read");
    assert!(ours(&[], None, None), "no new lines");
    assert!(ours(&session, Some(a), Some(a)));
    assert!(
        ours(&session, Some(a), None),
        "a child A killed before its summary, known by its store"
    );
    assert!(
        !ours(&session, Some(a), Some(other)),
        "a summary of another store"
    );
    assert!(!ours(&session, Some(other), None), "another store's open");
    assert!(!ours(&session, None, Some(a)), "no store.txt");
    session[0] = open(1, 4, true);
    assert!(!ours(&session, Some(a), Some(a)), "a replaying open first");
    session.remove(0);
    assert!(!ours(&session, Some(a), Some(a)), "a look line first");
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let dir = store_with(tmp.path(), "a", 1, 0, b"same");
    let id = Store::load(LabStore::open(dir.clone()).expect("the store"), BUILD)
        .expect("loads")
        .id();
    assert_eq!(store_id(&dir), Some(id), "store.txt names the store's id");
    assert_eq!(store_id(&tmp.path().join("none")), None);
}

#[test]
fn the_record_reads_every_pairs_head_and_names_each_broken_one() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let root = tmp.path().join("root");
    assert_eq!(heads(&root), Vec::new(), "no root, no pairs");
    // A sound pair with no lines yet: its head is its genesis (a ledger's own lines are the
    // lab's tests', which build them through a chain pin).
    let pair = Pair::Practice(0);
    drop(LabLog::open(&root, pair).expect("a writer"));
    let raw = LabLog::read_only(&root, pair).lines().expect("its lines");
    assert_eq!(raw, Vec::<Vec<u8>>::new());
    let mut log = LabLog::open(&root, Pair::Practice(3)).expect("a writer");
    log.append(0, b"not a line").expect("planted");
    drop(log);
    fs::write(root.join("practice-1"), b"").expect("a stray file, not a directory");
    fs::create_dir(root.join("pair-02")).expect("planted");
    let mut log = LabLog::open(&root, Pair::Real(2)).expect("a writer");
    log.append(0, b"x").expect("planted");
    drop(log);
    fs::write(root.join("pair-2/.DS_Store"), b"").expect("a stray file in a pair");
    let read = heads(&root);
    let names: Vec<&str> = read.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "pair-02",
            "pair-2",
            "practice-0",
            "practice-1",
            "practice-3"
        ]
    );
    assert_eq!(
        read[2].1,
        Ok(ledger_head(pair, &raw).expect("its head")),
        "a sound pair's head beside broken ones"
    );
    for (n, (name, head)) in read.iter().enumerate().filter(|(n, _)| *n != 2) {
        assert!(head.is_err(), "{n} {name}");
    }
    assert!(head_text(&read[0]).starts_with("pair-02 BROKEN: "));
    assert!(head_text(&read[1]).contains(".DS_Store"));
    assert!(head_text(&read[2]).starts_with("practice-0 head "));
    assert!(head_text(&read[4]).starts_with("practice-3 BROKEN: "));
    let file_root = tmp.path().join("a-file");
    fs::write(&file_root, b"").expect("planted");
    let read = heads(&file_root);
    assert_eq!(read.len(), 1, "a root that cannot be read is one line");
    assert!(head_text(&read[0]).starts_with("root BROKEN: "), "{read:?}");
}

#[test]
fn a_ledger_not_read_decides_neither_g2s_bits_nor_g3s_opens() {
    let mut i = finished();
    i.session = None;
    let g = decide(&i);
    assert_eq!(g.failed, Vec::new(), "no gate failed on lines never read");
    assert!(!g.decided);
    assert_eq!(g.line, None, "no line for a ledger not read");
    i.stores = Some(vec!["eval/1.rec differs between the stores".to_owned()]);
    assert_eq!(
        gates_of(&i),
        ["G2"],
        "the store comparison reads no ledger, so it still decides"
    );
    let wrote = Wrote::Nothing("the ledger could not be taken".to_owned());
    let mut i = finished();
    i.session = None;
    let g = decide(&i);
    let text = record_text(&i, &g, &wrote, &[]);
    assert!(
        text.contains("G2's bits and G3's open lines not decided: the pair's ledger was not read")
            && !text.contains("FAILED")
            && !text.contains("pass")
            && !text.contains("a child that did not finish"),
        "{text}"
    );
    assert!(sidecar_text(&i, &g, &wrote, &[]).contains("decided false\n"));
}

#[test]
fn an_interrupt_before_the_line_voids_a_finished_session() {
    let mut i = finished();
    i.stopped = true;
    let g = decide(&i);
    assert_eq!(g.failed, Vec::new());
    assert_eq!(g.line, Some(Verdict::Void(VoidCause::Other)));
    let wrote = Wrote::Void(VoidCause::Other, VoidClass::Closing);
    assert!(record_text(&i, &g, &wrote, &[]).contains(
        "stopped: an interrupt came before the runner's line; the session is void, by the cause \
         G5 names\n"
    ));
    for wrote in [
        Wrote::Nothing("the void line was refused: Io".to_owned()),
        Wrote::Nothing(
            "the pair's new lines are not this session's, so the runner writes nothing".to_owned(),
        ),
    ] {
        let text = record_text(&i, &g, &wrote, &[]);
        assert!(
            text.contains(
                "stopped: an interrupt came before the runner's line, and no void was written"
            ) && !text.contains("the session is void"),
            "a decided void that was not written:\n{text}"
        );
    }
    assert!(sidecar_text(&i, &g, &wrote, &[]).contains("stopped true\n"));
    assert!(sidecar_text(&finished(), &g, &wrote, &[]).contains("stopped false\n"));
    i.children[0].deadline = true;
    assert_eq!(
        decide(&i).line,
        Some(Verdict::Void(VoidCause::Deadline)),
        "a child's own cause first"
    );
    let mut early = finished();
    early.stopped = true;
    early.children[0].interrupted = true;
    early.children[0].summary = None;
    early.children[1] = Child {
        name: "B".to_owned(),
        ..Child::default()
    };
    early.session = Some(Vec::new());
    early.stores = None;
    let g = decide(&early);
    assert_eq!(g.line, None, "stopped before its open: no line");
    for (session, wrote) in [
        (
            Some(Vec::new()),
            Wrote::Nothing("the session stopped before its open".to_owned()),
        ),
        (
            None,
            Wrote::Nothing("the ledger could not be taken (LedgerBusy: interrupted)".to_owned()),
        ),
    ] {
        early.session = session;
        let text = record_text(&early, &decide(&early), &wrote, &[]);
        assert!(
            text.contains(
                "stopped: an interrupt came before the runner's line, and no void was written"
            ) && !text.contains("the session is void"),
            "{text}"
        );
    }
}

#[cfg(debug_assertions)]
#[test]
fn the_runners_files_are_made_new_and_flushed() {
    use aterm_durable::Flush;
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let path = tmp.path().join("record.txt");
    let rec = aterm_durable::seam::Recording::start();
    save(&path, "one").expect("saved");
    assert_eq!(rec.take(), [Flush::Durable], "flushed durably");
    assert_eq!(
        save(&path, "two").map_err(|e| e.kind()),
        Err(std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(fs::read_to_string(&path).expect("kept"), "one");
    let (mut o, _) = capture(tmp.path(), "A").expect("A's capture files");
    o.write_all(b"A's").expect("written");
    for planted in ["B.stdout", "C.stderr"] {
        fs::write(tmp.path().join(planted), b"kept").expect("planted");
    }
    for name in ["A", "B", "C"] {
        assert_eq!(
            capture(tmp.path(), name).map(|_| ()).map_err(|e| e.kind()),
            Err(std::io::ErrorKind::AlreadyExists),
            "{name}"
        );
    }
    assert_eq!(fs::read(tmp.path().join("A.stdout")).expect("kept"), b"A's");
    assert_eq!(
        fs::read(tmp.path().join("B.stdout")).expect("kept"),
        b"kept"
    );
    let _ = rec.take();
}

#[test]
fn each_gates_miss_after_the_children_exit_is_a_gate_void() {
    type Plant = fn(&mut Inputs);
    let cases: [(&str, Plant); 14] = [
        ("G0", |i| {
            let dirty = format!("{BUILD}-dirty");
            i.build.clone_from(&dirty);
            for c in &mut i.children {
                c.summary.as_mut().expect("its").build.clone_from(&dirty);
            }
        }),
        ("G0", |i| i.lab.1 = Some(Digest([6; 32]))),
        ("G0", |i| i.lab.1 = None),
        ("G0", |i| {
            i.children[1].summary.as_mut().expect("B's").build =
                "aterm-lab 0.1.0+gffffffffffff".to_owned()
        }),
        ("G0", |i| {
            i.children[0].summary.as_mut().expect("A's").profile = "release".to_owned()
        }),
        ("G2", |i| {
            i.children[1].summary.as_mut().expect("B's").bits = "10100".to_owned()
        }),
        ("G2", |i| lines(i)[2] = Line::LookVerdict { bit: true }),
        ("G2", |i| {
            i.stores = Some(vec!["eval/1.rec differs between the stores".to_owned()])
        }),
        ("G3", |i| {
            i.children[0].summary.as_mut().expect("A's").held_out_judged = 1
        }),
        ("G3", |i| lines(i)[7] = open(2, 6, true)),
        ("G3", |i| lines(i)[7] = open(2, 4, false)),
        ("G2", |i| i.stores = None),
        ("G4", |i| {
            i.children[1].summary.as_mut().expect("B's").footprint = Some(MAX_FOOTPRINT + 1)
        }),
        ("G4", |i| {
            i.children[0].summary.as_mut().expect("A's").footprint = None
        }),
    ];
    for (gate, plant) in cases {
        let mut i = finished();
        plant(&mut i);
        let g = decide(&i);
        assert_eq!(gates_of(&i), [gate], "{:?}", g.failed);
        assert_eq!(g.line, Some(Verdict::Void(VoidCause::Gate)), "{gate}");
    }
    let mut i = finished();
    i.children[1].summary.as_mut().expect("B's").footprint = Some(MAX_FOOTPRINT);
    assert_eq!(
        decide(&i).line,
        Some(Verdict::Record),
        "2 GiB itself is within G4"
    );
    let mut i = finished();
    lines(&mut i).remove(7);
    assert_eq!(gates_of(&i), ["G3"], "B opened no line on its store");
}

/// A store for practice pair 0 under `tmp/name`, with one eval file.
fn store_with(tmp: &Path, name: &str, nonce: u64, ledger: u8, eval: &[u8]) -> PathBuf {
    let dir = tmp.join(name);
    fs::create_dir(&dir).expect("its directory");
    let io = LabStore::open(dir.clone()).expect("the store");
    drop(Store::init(io, BUILD, Pair::Practice(0), nonce, Digest([ledger; 32])).expect("v0"));
    fs::create_dir(dir.join("eval")).expect("its eval directory");
    fs::write(dir.join("eval/1.rec"), eval).expect("an eval record");
    dir
}

#[test]
fn the_store_comparison_excludes_ids_and_ledger_heads_alone() {
    let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
    let a = store_with(tmp.path(), "a", 1, 0, b"same");
    let b = store_with(tmp.path(), "b", 2, 7, b"same");
    assert_eq!(compare_stores(&a, &b, BUILD), Ok(Vec::new()));
    let c = store_with(tmp.path(), "c", 3, 0, b"diff");
    assert_eq!(
        compare_stores(&a, &c, BUILD),
        Ok(vec!["eval/1.rec differs between the stores".to_owned()])
    );
    let d = store_with(tmp.path(), "d", 4, 0, b"same");
    fs::write(d.join("eval/2.rec"), b"more").expect("planted");
    assert_eq!(
        compare_stores(&a, &d, BUILD),
        Ok(vec![
            "the stores' eval files differ in their names".to_owned()
        ])
    );
    let e = store_with(tmp.path(), "e", 5, 0, b"same");
    let v0 = {
        let mut s =
            Store::load(LabStore::open(e.clone()).expect("the store"), BUILD).expect("loads");
        let v0 = s.pointer().active;
        s.rollback(v0, Digest([0; 32])).expect("a rollback");
        v0
    };
    assert_eq!(
        compare_stores(&a, &e, BUILD),
        Ok(vec![
            "the stores' revisions differ in (reason, active, head)".to_owned()
        ])
    );
    let f = store_with(tmp.path(), "f", 6, 0, b"same");
    fs::write(f.join(format!("versions/{}/state", v0.hex())), b"x").expect("planted");
    assert!(
        compare_stores(&a, &f, BUILD).is_err(),
        "a store that does not load is no comparison"
    );
    let bare = |name: &str, nonce: u64| {
        let dir = tmp.path().join(name);
        fs::create_dir(&dir).expect("its directory");
        let io = LabStore::open(dir.clone()).expect("the store");
        drop(Store::init(io, BUILD, Pair::Practice(0), nonce, Digest([0; 32])).expect("v0"));
        dir
    };
    let (x, y) = (bare("bare-a", 8), bare("bare-b", 9));
    assert!(
        compare_stores(&x, &y, BUILD)
            .is_ok_and(|d| d.contains(&"store A holds no eval files".to_owned())),
        "a comparison of nothing is no pass"
    );
    assert!(
        compare_stores(&a, &a, BUILD).is_err(),
        "a store compared with itself is refused by its own lock"
    );
}

#[test]
fn the_record_claims_only_a_real_pairs_session_of_record() {
    let i = finished();
    let g = decide(&i);
    let text = record_text(&i, &g, &Wrote::Record, &[]);
    assert!(
        text.contains("marks: no claim"),
        "a practice pair claims nothing:\n{text}"
    );
    assert!(text.contains("G5: record"));
    assert!(text.contains("gates: G0 G2 G3 G4 pass"));
    assert!(
        !text.contains("not read")
            && !text.contains("not decided")
            && !text.contains("an interrupt came"),
        "a finished, uninterrupted session leaves nothing unread:\n{text}"
    );
    assert!(text.contains("child A: retained 1000 bytes in 5 versions"));
    let mut real = finished();
    real.pair = Pair::Real(1);
    let g = decide(&real);
    assert!(
        record_text(&real, &g, &Wrote::Record, &[])
            .contains("marks: claimed: C0, C4, C5, and each criterion")
    );
    let refused = record_text(
        &real,
        &g,
        &Wrote::Nothing("the record line was refused: Io".to_owned()),
        &[],
    );
    assert!(
        refused.contains("marks: no claim"),
        "no record line, no claim:\n{refused}"
    );
    let mut bare = real.clone();
    bare.children[0].summary.as_mut().expect("A's").retained = None;
    let g = decide(&bare);
    assert!(
        record_text(&bare, &g, &Wrote::Record, &[]).contains("not C4"),
        "C4 needs its retained bytes printed"
    );
    let mut failed = real.clone();
    failed.children[0].exit = Some(1);
    failed.children[1] = Child {
        name: "B".to_owned(),
        ..Child::default()
    };
    failed.stores = None;
    let g = decide(&failed);
    assert_eq!(g.line, Some(Verdict::Void(VoidCause::ChildFailed)));
    let text = record_text(
        &failed,
        &g,
        &Wrote::Void(VoidCause::ChildFailed, VoidClass::Closing),
        &[],
    );
    assert!(
        !text.contains("G2 G3 G4 pass")
            && text.contains("G2 not decided")
            && !text.contains("G3's guard not read for child A"),
        "A left a summary, so its guard was read:\n{text}"
    );
    assert!(text.contains("marks: no claim"), "{text}");
    let mut b_failed = real.clone();
    b_failed.children[1].exit = Some(1);
    b_failed.stores = None;
    let g = decide(&b_failed);
    assert_eq!(g.line, Some(Verdict::Void(VoidCause::ChildFailed)));
    assert!(!g.decided, "B did not finish");
    assert_eq!(
        g.failed,
        Vec::new(),
        "G2 is not decided, so the uncompared stores are no miss"
    );
    let text = record_text(
        &b_failed,
        &g,
        &Wrote::Void(VoidCause::ChildFailed, VoidClass::Closing),
        &[],
    );
    assert!(
        text.contains("G2 not decided") && !text.contains("G3's guard not read"),
        "B did not finish, so G2 was not decided, and both guards were read:\n{text}"
    );
    // A guard that read a held-out item is the child's own failure (its exit passes the guard,
    // so it exits 1): G3 fails, read from the summary the child left, and the child's own end
    // names the void.
    for n in [0, 1] {
        let mut read = real.clone();
        read.children[n].exit = Some(1);
        read.children[n]
            .summary
            .as_mut()
            .expect("its")
            .held_out_judged = 1;
        if n == 0 {
            read.children[1] = Child {
                name: "B".to_owned(),
                ..Child::default()
            };
        }
        read.stores = None;
        let name = ["A", "B"][n];
        let g = decide(&read);
        assert_eq!(
            g.failed,
            [("G3", format!("child {name} read held_out_judged 1"))],
            "{name}"
        );
        assert_eq!(g.line, Some(Verdict::Void(VoidCause::ChildFailed)));
        let text = record_text(
            &read,
            &g,
            &Wrote::Void(VoidCause::ChildFailed, VoidClass::Closing),
            &[],
        );
        assert!(
            text.contains(&format!("G3 FAILED: child {name} read held_out_judged 1\n"))
                && !text.contains(&format!("G3's guard not read for child {name}")),
            "{text}"
        );
    }
    let mut control = finished();
    control.pair = Pair::Real(1);
    control.children[0].summary.as_mut().expect("A's").bits = "11110".to_owned();
    control.children[1].summary.as_mut().expect("B's").bits = "11110".to_owned();
    lines(&mut control)[2] = Line::LookVerdict { bit: true };
    let g = decide(&control);
    assert_eq!(g.line, Some(Verdict::Record));
    assert!(record_text(&control, &g, &Wrote::Record, &[]).contains("C5 not shown"));
    let mut void = finished();
    void.pair = Pair::Real(1);
    void.lab.1 = None;
    let g = decide(&void);
    let wrote = Wrote::Void(VoidCause::Gate, VoidClass::Closing);
    let heads: Vec<Head> = vec![
        ("pair-1".to_owned(), Ok(Digest([0xab; 32]))),
        ("practice-0".to_owned(), Err("a broken link".to_owned())),
    ];
    let text = record_text(&void, &g, &wrote, &heads);
    assert!(
        text.contains(&format!(
            "ledger pair-1 head {}\n",
            Digest([0xab; 32]).hex()
        )) && text.contains("ledger practice-0 BROKEN: a broken link\n"),
        "each pair's ledger head:\n{text}"
    );
    assert!(
        text.contains("marks: no claim")
            && text.contains("G0 FAILED")
            && text.contains("G5: void Gate (closing: the policy version is closed)"),
        "{text}"
    );
    let side = sidecar_text(&void, &g, &wrote, &heads);
    assert!(side.contains(&format!(
        "ledger pair-1 head {}\n",
        Digest([0xab; 32]).hex()
    )));
    assert_eq!(side.lines().next(), Some(SIDECAR));
    assert!(side.contains("failed G0 the copied lab changed during the session"));
    assert!(side.contains("retained=1000/5"));
    assert!(side.ends_with("line void Gate (closing: the policy version is closed)\n"));
}

#[test]
fn the_runner_takes_its_flags_alone() {
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        parse(&args(&[
            "--lab",
            "l",
            "--out",
            "o",
            "--practice",
            "0",
            "--tests-log",
            "t"
        ]))
        .ok(),
        Some(Args {
            lab: "l".to_owned(),
            out: "o".to_owned(),
            pair: Pair::Practice(0),
            tests_log: "t".to_owned(),
        })
    );
    for bad in [
        &["--out", "o", "--practice", "0", "--tests-log", "t"][..],
        &["--lab", "l", "--practice", "0", "--tests-log", "t"],
        &["--lab", "l", "--out", "o", "--practice", "0"],
        &["--lab", "l", "--out", "o", "--tests-log", "t"],
        &[
            "--lab",
            "l",
            "--out",
            "o",
            "--practice",
            "00",
            "--tests-log",
            "t",
        ],
        &[
            "--lab",
            "l",
            "--out",
            "o",
            "--practice",
            "0",
            "--tests-log",
            "t",
            "x",
        ],
    ] {
        assert_eq!(parse(&args(bad)).err().map(|f| f.code), Some(2), "{bad:?}");
    }
    assert_eq!(
        parse_stamp(&format!("build {BUILD}\nprofile debug\n")),
        Some((BUILD.to_owned(), "debug".to_owned()))
    );
    for bad in [
        "",
        "build x\n",
        "profile debug\nbuild x\n",
        "build x\nprofile y\nmore\n",
    ] {
        assert_eq!(parse_stamp(bad), None, "{bad:?}");
    }
    let mut help = Vec::new();
    assert_eq!(cmd_to(&args(&["--help"]), &mut help).ok(), Some(0));
    assert_eq!(help, HELP.as_bytes());
    /// A stdout that is gone: every write and flush fails as a dead pipe does.
    struct Gone;
    impl Write for Gone {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }
    assert_eq!(
        cmd_to(&args(&["--help"]), &mut Gone).ok(),
        Some(0),
        "a stdout that is gone is no panic, and the exit still passes the guard"
    );
}
