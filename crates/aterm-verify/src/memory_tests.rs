// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a stage claims from the peaks kept for it ([`super::need`]), and the record they are
//! kept in ([`super::Records`]), on fixed records: no test reads this user's own.

use super::*;
use crate::plan::{Lane, StageId, StageSpec};
use crate::scope::Scope;

const GIB: u64 = 1 << 30;

#[test]
fn cpu_observations_survive_exit_without_double_counting_or_hiding_missing_reads() {
    let meter = Meter::new();
    assert_eq!(meter.observed_cpu(), None);
    meter.started();
    meter.book(10, &[]);
    meter.ended(10);
    assert_eq!(
        meter.observed_cpu(),
        None,
        "an unobserved child is not zero CPU"
    );
    let process = |pid, start, cpu_ns| aterm_sysprobe::MemberUsage {
        pid,
        start,
        cpu_ns,
        ..Default::default()
    };
    meter.book(10, &[process(10, 1, 100), process(11, 2, 200)]);
    meter.book(10, &[process(10, 1, 150)]);
    meter.ended(10);
    // A repeated counter, even under another group, is not additional work.
    meter.book(20, &[process(11, 2, 190), process(10, 3, 75)]);
    assert_eq!(
        meter.observed_cpu(),
        Some(ObservedCpu {
            nanoseconds: 425,
            processes: 3
        })
    );
    let sibling = Meter::new();
    sibling.book(30, &[process(30, 4, 999)]);
    assert_eq!(
        meter.observed_cpu().unwrap().nanoseconds,
        425,
        "parallel stages are separate"
    );
}

fn spec(id: StageId, lane: Lane) -> StageSpec {
    StageSpec {
        id,
        title: format!("{id:?}"),
        lane,
        exclusive: false,
        after_stages: Vec::new(),
        mem: 0,
        part: None,
    }
}

fn compile() -> StageSpec {
    spec(StageId::TestCompile, Lane::MainTarget)
}

fn gui() -> Scope {
    Scope::crate_only("aterm-gui")
}

fn packages(names: &[&str]) -> Scope {
    Scope::packages(&names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())
}

/// The key `spec` keeps its record under, run over `scope` ([`StageKey::of`]).
fn over(spec: &StageSpec, scope: &Scope) -> Key {
    Key::of(spec.id, scope)
}

/// Records holding `peaks` (GiB, oldest first) for `id` over `scope`.
fn kept(id: StageId, scope: &Scope, peaks: &[u64]) -> Records {
    let mut r = Records::default();
    for (at, gib) in peaks.iter().enumerate() {
        r.push(Key::of(id, scope), gib * GIB, at as u64);
    }
    r
}

/// A stage claims the most of the peaks kept for it and a fifth more, at least 1 GiB more: of
/// 10 and 4 GiB kept (the heavy one older), 12 GiB; of 2 GiB, 3 GiB. NEGATIVE CONTROL: the newest
/// peak alone (4 GiB) would claim 5 GiB, and a claim without the margin 10 GiB.
#[test]
fn a_stage_claims_the_most_it_kept_and_a_fifth_more() {
    let heavy = kept(StageId::TestCompile, &gui(), &[10, 4]);
    let n = need(
        &heavy,
        &Records::default(),
        &compile(),
        &over(&compile(), &gui()),
        Some(24 * GIB),
    );
    assert_eq!(n.bytes, 12 * GIB, "{n}");
    assert_eq!(
        n.how,
        How::Learned(Learned {
            peak: 10 * GIB,
            runs: 2,
            wider: None
        })
    );
    assert!(n.to_string().contains("last 2 run(s)"), "{n}");
    let light = kept(StageId::TestCompile, &gui(), &[2]);
    assert_eq!(
        need(
            &light,
            &Records::default(),
            &compile(),
            &over(&compile(), &gui()),
            Some(24 * GIB)
        )
        .bytes,
        3 * GIB
    );
}

/// With no peak kept, a stage that compiles claims 16 GiB, at most half of physical memory, and
/// any other stage claims nothing; a stage that touches no target directory claims nothing even
/// with a peak kept for it. NEGATIVE CONTROL: the compiling stage's own record, once kept, is
/// what it claims instead of the cold figure.
#[test]
fn with_nothing_kept_a_compiling_stage_claims_the_cold_figure() {
    let none = Records::default();
    for (physical, want) in [
        (Some(24 * GIB), 12 * GIB),
        (Some(48 * GIB), 16 * GIB),
        (Some(8 * GIB), 4 * GIB),
        (None, 16 * GIB),
    ] {
        let n = need(
            &none,
            &Records::default(),
            &compile(),
            &over(&compile(), &gui()),
            physical,
        );
        assert_eq!((n.bytes, &n.how), (want, &How::Cold), "{physical:?}");
    }
    let run = spec(StageId::Test, Lane::MainTarget);
    let n = need(
        &none,
        &Records::default(),
        &run,
        &over(&run, &gui()),
        Some(24 * GIB),
    );
    assert_eq!((n.bytes, n.how), (0, How::Light));
    let fmt = spec(StageId::Formatting, Lane::Pure);
    let pure = kept(StageId::Formatting, &gui(), &[3]);
    let n = need(
        &pure,
        &Records::default(),
        &fmt,
        &over(&fmt, &gui()),
        Some(24 * GIB),
    );
    assert_eq!((n.bytes, n.how), (0, How::Unmetered));
    let warm = kept(StageId::TestCompile, &gui(), &[5]);
    assert_eq!(
        need(
            &warm,
            &Records::default(),
            &compile(),
            &over(&compile(), &gui()),
            Some(24 * GIB)
        )
        .bytes,
        6 * GIB
    );
}

/// A record is read for its own stage and selection only: another selection's 15 GiB is not
/// this one's, nor another stage's. NEGATIVE CONTROL: the same record under the same selection
/// and stage is read.
#[test]
fn another_selections_record_is_not_read() {
    let r = kept(StageId::TestCompile, &gui(), &[15]);
    assert_eq!(
        r.learned(&Key::of(
            StageId::TestCompile,
            &Scope::crate_only("aterm-grid")
        )),
        None
    );
    assert_eq!(r.learned(&Key::of(StageId::Tippy, &gui())), None);
    assert_eq!(
        r.learned(&Key::of(StageId::TestCompile, &gui()))
            .map(|l| l.peak),
        Some(15 * GIB)
    );
}

/// A selection with no record of its own reads the most of the wider selections' peaks kept in
/// its checkout, under its variant: a superset's build compiles the subset's crates too, and a
/// wider run over a warmer cache can have taken less than this one will. The workspace holds
/// every selection; `-p a -p b` holds `-p a` and `-p b` but not `-p d`; a selection's own record
/// wins over any wider one, and the workspace reads only its own. The order the crates were
/// named in makes no other key. NEGATIVE CONTROL: `-p a`, held by `-p a -p b` (15 GiB) and `-p a
/// -p c` (10 GiB), reads 15, not the least; a narrower selection's peak never bounds a wider one;
/// and a wider record of another checkout or another variant is not read.
#[test]
fn a_wider_selections_most_peak_bounds_a_narrower_one() {
    let mut r = Records::default();
    let key = |names: &[&str]| Key::of(StageId::TestCompile, &packages(names));
    let workspace = Key::of(StageId::TestCompile, &Scope::workspace());
    r.push(key(&["b", "a"]), 15 * GIB, 2);
    r.push(key(&["a", "c"]), 10 * GIB, 3);
    let read = |r: &Records, k: Key| r.learned(&k).map(|l| (l.peak, l.wider));
    assert_eq!(
        read(&r, key(&["a"])),
        Some((15 * GIB, Some(key(&["a", "b"]))))
    );
    assert_eq!(
        read(&r, key(&["c"])),
        Some((10 * GIB, Some(key(&["a", "c"]))))
    );
    assert_eq!(read(&r, key(&["d"])), None);
    r.push(workspace.clone(), 30 * GIB, 1);
    assert_eq!(
        read(&r, key(&["d"])),
        Some((30 * GIB, Some(workspace.clone())))
    );
    assert_eq!(
        read(&r, key(&["a"])),
        Some((30 * GIB, Some(workspace.clone()))),
        "the workspace's is the most"
    );
    assert_eq!(read(&r, key(&["a", "b"])), Some((15 * GIB, None)));
    r.push(key(&["a"]), 2 * GIB, 4);
    assert_eq!(read(&r, key(&["a"])), Some((2 * GIB, None)));
    let mut narrow = Records::default();
    narrow.push(key(&["a"]), 2 * GIB, 1);
    assert_eq!(narrow.learned(&key(&["a", "b"])), None);
    assert_eq!(narrow.learned(&workspace), None);
    let n = need(
        &r,
        &Records::default(),
        &compile(),
        &over(&compile(), &packages(&["c"])),
        Some(24 * GIB),
    );
    assert!(
        n.to_string()
            .contains("the most a run over a wider selection (the workspace)"),
        "{n}"
    );
    let mut apart = Records::default();
    apart.push(
        Key {
            checkout: "/elsewhere".to_owned(),
            ..key(&["a", "b"])
        },
        20 * GIB,
        1,
    );
    apart.push(
        Key {
            variant: Some("jobs 4".to_owned()),
            ..key(&["a", "b"])
        },
        20 * GIB,
        2,
    );
    assert_eq!(
        apart.learned(&key(&["a"])),
        None,
        "another checkout or variant"
    );
}

/// A stage with no record in its checkout, a new worktree's first run, claims by what its stage
/// and selection kept in another checkout: the most any other checkout keeps (15 GiB of /vmem's
/// 15 and 10, beside /other's 12), and a fifth more. A record in its own checkout, its own or a
/// wider selection's, wins over another checkout's; another checkout's record of a wider
/// selection, another stage or another variant (30 GiB each) is not read. NEGATIVE CONTROL: read
/// in its own checkout alone, as before, the new worktree's compile claimed the cold 12 GiB.
#[test]
fn a_new_checkout_claims_what_another_checkout_kept() {
    let at = |checkout: &str, scope: &Scope| Key {
        checkout: checkout.to_owned(),
        ..over(&compile(), scope)
    };
    let wider = packages(&["aterm-gui", "aterm-grid"]);
    let mut r = Records::default();
    r.push(at("/vmem", &gui()), 15 * GIB, 1);
    r.push(at("/vmem", &gui()), 10 * GIB, 2);
    r.push(at("/other", &gui()), 12 * GIB, 3);
    r.push(at("/far", &wider), 30 * GIB, 4);
    r.push(
        Key {
            variant: Some("jobs 4".to_owned()),
            ..at("/far", &gui())
        },
        30 * GIB,
        5,
    );
    r.push(
        Key {
            checkout: "/far".to_owned(),
            ..Key::of(StageId::Tippy, &gui())
        },
        30 * GIB,
        6,
    );
    let n = need(
        &r,
        &Records::default(),
        &compile(),
        &at("/new", &gui()),
        Some(24 * GIB),
    );
    assert_eq!(
        (n.bytes, &n.how),
        (
            aterm_capacity::peak_claim(15 * GIB),
            &How::Elsewhere(Learned {
                peak: 15 * GIB,
                runs: 2,
                wider: Some(at("/vmem", &gui())),
            })
        ),
        "{n}"
    );
    assert!(
        n.to_string()
            .contains("the most a run of it in /vmem used (15 GiB)"),
        "{n}"
    );
    let mut wide = r.clone();
    wide.push(at("/new", &wider), 8 * GIB, 7);
    let n = need(
        &wide,
        &Records::default(),
        &compile(),
        &at("/new", &gui()),
        Some(24 * GIB),
    );
    assert_eq!(
        (n.bytes, n.how),
        (
            aterm_capacity::peak_claim(8 * GIB),
            How::Learned(Learned {
                peak: 8 * GIB,
                runs: 1,
                wider: Some(at("/new", &wider)),
            })
        ),
        "a wider selection's in its own checkout"
    );
    r.push(at("/new", &gui()), 5 * GIB, 8);
    let n = need(
        &r,
        &Records::default(),
        &compile(),
        &at("/new", &gui()),
        Some(24 * GIB),
    );
    assert_eq!(
        (n.bytes, n.how),
        (
            aterm_capacity::peak_claim(5 * GIB),
            How::Learned(Learned {
                peak: 5 * GIB,
                runs: 1,
                wider: None,
            })
        ),
        "its own"
    );
}

/// A stage verify keeps no run of anywhere claims by what a plain `targo` build of the same work
/// kept (BUILD-ADMIT's book): the test compile over `-p aterm-gui` by the most of a plain
/// `targo test -p aterm-gui` (15.6 GiB in /badmit) and a `--tests` one (16 GiB in /other), and a
/// fifth more; the lint by a plain `targo tippy --all-targets`. A plain build of less work or
/// other work is never read: `--lib`, `--release`, `--features`, `targo build`, another
/// selection, a lint of the default targets (30 GiB each). Verify's own record of it, in another
/// checkout, wins over the plain book. NEGATIVE CONTROL: with the plain book unread, the test
/// compile claimed the cold 12 GiB.
#[test]
fn a_stage_verify_never_ran_claims_what_a_plain_build_of_it_kept() {
    let plain_key = |stage: &str, checkout: &str, crates: &[&str], variant: Option<&str>| Key {
        stage: stage.to_owned(),
        checkout: checkout.to_owned(),
        crates: Some(crates.iter().map(|c| (*c).to_owned()).collect()),
        variant: variant.map(str::to_owned),
    };
    let mut plain = Records::default();
    let test = plain_key("targo test", "/badmit", &["aterm-gui"], None);
    plain.push(test.clone(), 15 * GIB, 1);
    plain.push(test.clone(), 14 * GIB, 2);
    let tests = plain_key("targo test", "/other", &["aterm-gui"], Some("--tests"));
    plain.push(tests.clone(), 16 * GIB, 3);
    for (stage, crates, variant) in [
        ("targo test", &["aterm-gui"][..], Some("--lib")),
        ("targo test", &["aterm-gui"][..], Some("release")),
        (
            "targo test",
            &["aterm-gui"][..],
            Some("--tests, features x"),
        ),
        ("targo build", &["aterm-gui"][..], None),
        ("targo test", &["aterm-gui", "aterm-grid"][..], None),
        ("targo tippy", &["aterm-gui"][..], None),
    ] {
        plain.push(plain_key(stage, "/far", crates, variant), 30 * GIB, 4);
    }
    let lint = plain_key("targo tippy", "/far", &["aterm-gui"], Some("--all-targets"));
    plain.push(lint.clone(), 9 * GIB, 5);
    let here = |id: StageId| Key {
        checkout: "/new".to_owned(),
        ..Key::of(id, &gui())
    };
    let n = need(
        &Records::default(),
        &plain,
        &compile(),
        &here(StageId::TestCompile),
        Some(24 * GIB),
    );
    assert_eq!(
        (n.bytes, &n.how),
        (
            aterm_capacity::peak_claim(16 * GIB),
            &How::Plain(Learned {
                peak: 16 * GIB,
                runs: 1,
                wider: Some(tests),
            })
        ),
        "{n}"
    );
    assert!(
        n.to_string()
            .contains("the most a plain `targo test` of it in /other used (16 GiB)"),
        "{n}"
    );
    let tippy = spec(StageId::Tippy, Lane::TippyTarget);
    let n = need(
        &Records::default(),
        &plain,
        &tippy,
        &here(StageId::Tippy),
        Some(24 * GIB),
    );
    assert_eq!(
        (n.bytes, n.how),
        (
            aterm_capacity::peak_claim(9 * GIB),
            How::Plain(Learned {
                peak: 9 * GIB,
                runs: 1,
                wider: Some(lint),
            })
        )
    );
    let mut verify = Records::default();
    verify.push(
        Key {
            checkout: "/vmem".to_owned(),
            ..Key::of(StageId::TestCompile, &gui())
        },
        5 * GIB,
        6,
    );
    let n = need(
        &verify,
        &plain,
        &compile(),
        &here(StageId::TestCompile),
        Some(24 * GIB),
    );
    assert!(matches!(n.how, How::Elsewhere(_)), "{n}");
    assert_eq!(n.bytes, 6 * GIB);
    let tagged = Key {
        variant: Some("after -p aterm-grid".to_owned()),
        ..here(StageId::TestCompile)
    };
    let n = need(
        &Records::default(),
        &plain,
        &compile(),
        &tagged,
        Some(24 * GIB),
    );
    assert_eq!(
        n.how,
        How::Cold,
        "a variant verify keeps is no plain build's"
    );
}

/// A run keys each stage's record by its checkout, its selection where the stage reads it, and
/// its variant ([`StageKey::of_run`]): the test compile over `-p aterm-gui` in `/ck` by that crate
/// in `/ck`; the export, whose work is the same whatever is selected, by the workspace; the test
/// run by the test binaries it runs at once; the cells asked for by their sorted names, and
/// cells a selection pulled by its crates. NEGATIVE CONTROL: the same stages in another checkout
/// key apart.
#[test]
fn a_run_keys_each_stage_by_its_checkout_selection_and_variant() {
    let ctx = |root: &str| {
        crate::Ctx::new(
            std::path::PathBuf::from(root),
            gui(),
            crate::EnvSnapshot::default(),
            std::path::PathBuf::from("/tmp"),
        )
    };
    let here = ctx("/ck");
    let compile = Key::of_run(StageId::TestCompile, &here);
    assert_eq!(
        (
            compile.checkout.as_str(),
            compile.crates.as_deref(),
            &compile.variant
        ),
        ("/ck", Some(&["aterm-gui".to_owned()][..]), &None)
    );
    let export = Key::of_run(StageId::Export, &here);
    assert_eq!((export.crates, export.variant), (None, None));
    assert_eq!(
        Key::of_run(StageId::Test, &here).variant,
        Some(format!("jobs {}", here.test_jobs()))
    );
    let checker = Key::of_run(StageId::SpecCheckers, &here);
    assert_eq!(checker.variant, Some(format!("jobs {}", here.test_jobs())));
    assert_eq!(
        checker.crates,
        Some(vec!["aterm-gui".to_owned(), "aterm-spec".to_owned()])
    );
    assert_eq!(
        Key::of_run(StageId::CheckersCompile, &here).crates,
        checker.crates
    );
    let mut cells = ctx("/ck");
    cells.cells = vec!["win".to_owned(), "linux-arm".to_owned()];
    let foreign = Key::of_run(StageId::ForeignCells, &cells);
    assert_eq!(
        (foreign.crates, foreign.variant),
        (
            Some(vec!["aterm-gui".to_owned()]),
            Some("cells linux-arm win".to_owned())
        ),
        "`-p` narrows a named `cells`, so its small peak stays off the whole graph's record"
    );
    let pulled = Key::of_run(StageId::ForeignCells, &ctx("/ck").with_cells_pulled(true));
    assert_eq!(
        (pulled.crates, pulled.variant),
        (
            Some(vec!["aterm-gui".to_owned()]),
            Some("cells pulled".to_owned())
        )
    );
    assert_ne!(Key::of_run(StageId::TestCompile, &ctx("/other")), compile);
    assert_ne!(
        Key::of_run(StageId::Export, &ctx("/other")),
        Key::of_run(StageId::Export, &here)
    );
}

/// A light run does not hide the heavy one before it while the record keeps it: after a 14 GiB
/// peak and seven light ones the record still reads 14 GiB; the eighth light one lets it go.
/// The file keeps at most [`ROWS_KEPT`] records, losing the one whose newest peak is oldest.
/// NEGATIVE CONTROL: the seven light peaks alone read 1 GiB.
#[test]
fn a_light_run_does_not_hide_a_heavy_one_it_keeps() {
    let key = Key::of(StageId::Tippy, &gui());
    let mut r = Records::default();
    r.push(key.clone(), 14 * GIB, 0);
    for at in 1..8 {
        r.push(key.clone(), GIB, at);
    }
    assert_eq!(
        r.learned(&key).map(|l| (l.peak, l.runs)),
        Some((14 * GIB, 8))
    );
    r.push(key.clone(), GIB, 8);
    assert_eq!(r.learned(&key).map(|l| (l.peak, l.runs)), Some((GIB, 8)));
    let light = kept(StageId::Tippy, &gui(), &[1, 1, 1, 1, 1, 1, 1]);
    assert_eq!(light.learned(&key).map(|l| l.peak), Some(GIB));

    let mut many = Records::default();
    for n in 0..=ROWS_KEPT {
        let at = u64::try_from(n).expect("small");
        many.push(
            Key::of(StageId::Tippy, &packages(&[n.to_string().as_str()])),
            GIB,
            at,
        );
    }
    assert_eq!(many.rows().len(), ROWS_KEPT);
    assert!(
        many.learned(&Key::of(StageId::Tippy, &packages(&["0"])))
            .is_none(),
        "the oldest record went"
    );
}

/// A compiling stage's book keeps nothing for a run light beside its heaviest peak of the last
/// two weeks: after a 16 GiB test compile, a 2 GiB one (its units were fresh) is not kept and the
/// next run still claims by 16 GiB, while a 5 GiB one is. A stage that compiles nothing keeps
/// every run: the test run's light peak after a heavy one. NEGATIVE CONTROL: with the compile
/// kept as the test run is, the 2 GiB run was kept.
#[test]
fn a_compile_light_beside_its_heavy_peak_keeps_nothing() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let book = Book::at(
        Some(dir.path().join("stage-memory.json")),
        Some(24 * GIB),
        Some(21 * GIB),
    );
    let key = over(&compile(), &gui());
    assert_eq!(
        book.keep(&compile(), &key, 16 * GIB).expect("kept"),
        Kept::Kept
    );
    assert_eq!(
        book.keep(&compile(), &key, 2 * GIB).expect("read"),
        Kept::Light
    );
    assert_eq!(
        book.records().learned(&key).map(|l| (l.peak, l.runs)),
        Some((16 * GIB, 1))
    );
    assert_eq!(
        book.keep(&compile(), &key, 5 * GIB).expect("kept"),
        Kept::Kept
    );
    let run = spec(StageId::Test, Lane::MainTarget);
    let run_key = over(&run, &gui());
    assert_eq!(
        book.keep(&run, &run_key, 16 * GIB).expect("kept"),
        Kept::Kept
    );
    assert_eq!(book.keep(&run, &run_key, GIB).expect("kept"), Kept::Kept);
    assert_eq!(book.records().learned(&run_key).map(|l| l.runs), Some(2));
    let fmt = spec(StageId::Formatting, Lane::Pure);
    assert_eq!(
        book.keep(&fmt, &over(&fmt, &gui()), GIB).expect("nothing"),
        Kept::Nowhere
    );
}

/// A SHAPED CALL KEEPS NO RECORD OF A RUN THAT COMPILED ALMOST NONE OF ITS WORK, even as the
/// first under its key: a cut planned by 13.02 GiB keeps neither a fresh run's 31 MiB nor a 3 GiB
/// one under a key that holds nothing, and keeps a 13 GiB one; a call planned by nothing (the
/// closing call claiming the whole estimate) keeps its first run, as any stage does. NEGATIVE
/// CONTROL, by hand: with the planned peak unread, the 31 MiB run was kept, the record's first.
#[test]
fn a_shaped_call_keeps_no_run_light_beside_what_it_was_planned_by() {
    use crate::shape::{Part, Piece};
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let book = Book::at(
        Some(dir.path().join("stage-memory.json")),
        Some(24 * GIB),
        Some(21 * GIB),
    );
    let cut = |planned: Option<u64>| StageSpec {
        part: Some(Part {
            piece: Piece::of(&["aterm-agent".to_owned()], false),
            index: 0,
            of: 2,
            after: Vec::new(),
            said: String::new(),
            planned,
            refused: Vec::new(),
        }),
        ..compile()
    };
    let key = over(&compile(), &packages(&["aterm-agent"]));
    let planned = cut(Some(1302 * GIB / 100));
    assert_eq!(
        book.keep(&planned, &key, 31 << 20).expect("read"),
        Kept::LightBeside(1302 * GIB / 100)
    );
    assert_eq!(
        book.keep(&planned, &key, 3 * GIB).expect("read"),
        Kept::LightBeside(1302 * GIB / 100)
    );
    assert_eq!(book.records().learned(&key), None, "nothing is kept");
    assert_eq!(
        book.keep(&planned, &key, 13 * GIB).expect("kept"),
        Kept::Kept
    );
    let other = over(&compile(), &packages(&["aterm-types"]));
    assert_eq!(
        book.keep(&cut(None), &other, 31 << 20).expect("kept"),
        Kept::Kept
    );
    assert_eq!(
        book.records().learned(&key).map(|l| (l.peak, l.runs)),
        Some((13 * GIB, 1))
    );
}

/// A call of a shaped test compile keeps its record over its own crates under its variant
/// ([`StageKey::of_stage`]): the first call, a cut that follows no call, under `verify -p
/// <its crates>`'s own key, so a run of that selection unshaped reads it and it reads theirs; a
/// lib-test call under `targets --lib`; the closing call under `after <the cuts>`, so the rest
/// of the work never stands for the whole of it. An unshaped test compile, a part of one call,
/// keeps under [`StageKey::of_run`]. NEGATIVE CONTROL: the closing call keyed as the stage
/// unshaped would read the whole run's record.
#[test]
fn a_shaped_calls_record_is_kept_under_its_own_crates_and_variant() {
    use crate::shape::{Part, Piece};
    let names = |c: &[&str]| c.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    let whole = ["aterm-agent", "aterm-gui", "aterm-types"];
    let ctx = crate::Ctx::new(
        std::path::PathBuf::from("/ck"),
        packages(&whole),
        crate::EnvSnapshot::default(),
        std::path::PathBuf::from("/tmp"),
    );
    let cut = Piece::of(&names(&["aterm-gui"]), false);
    let lib = Piece::of(&names(&["aterm-agent"]), true);
    let closing = Piece::of(&names(&whole), false);
    let part = |piece: &Piece, index: usize, after: &[&Piece]| StageSpec {
        part: Some(Part {
            piece: piece.clone(),
            index,
            of: 3,
            after: after.iter().map(|p| (*p).clone()).collect(),
            said: String::new(),
            planned: None,
            refused: Vec::new(),
        }),
        ..compile()
    };
    assert_eq!(
        Key::of_stage(&part(&cut, 0, &[]), &ctx),
        Key {
            checkout: "/ck".to_owned(),
            ..Key::of(StageId::TestCompile, &gui())
        }
    );
    assert_eq!(
        Key::of_stage(&part(&lib, 1, &[]), &ctx).variant.as_deref(),
        Some("targets --lib")
    );
    let last = Key::of_stage(&part(&closing, 2, &[&cut, &lib]), &ctx);
    assert_eq!(
        (last.crates, last.variant.as_deref()),
        (
            Some(names(&whole)),
            Some("after -p aterm-gui; -p aterm-agent --lib")
        )
    );
    let mut alone = part(&closing, 0, &[]);
    if let Some(p) = alone.part.as_mut() {
        p.of = 1;
    }
    assert_eq!(
        Key::of_stage(&alone, &ctx),
        Key::of_run(StageId::TestCompile, &ctx)
    );
    assert_ne!(
        Key::of_stage(&part(&closing, 2, &[&cut, &lib]), &ctx),
        Key::of_run(StageId::TestCompile, &ctx)
    );
}

/// A book keeps what a metered stage took, and the next run of it claims by it; it keeps nothing
/// for a stage that touches no target directory, nor with no file. Its budget is what can ever be
/// available, unbounded where that is unread. NEGATIVE CONTROL: before it keeps anything the
/// compile claims the cold figure.
#[test]
fn a_book_keeps_a_metered_stages_peak_for_its_next_run() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("stage-memory.json");
    let book = Book::at(Some(path.clone()), Some(24 * GIB), Some(21 * GIB));
    assert_eq!(
        book.need(&compile(), &over(&compile(), &gui())).how,
        How::Cold
    );
    book.keep(&compile(), &over(&compile(), &gui()), 5 * GIB)
        .expect("kept");
    assert_eq!(
        book.need(&compile(), &over(&compile(), &gui())).bytes,
        6 * GIB
    );
    let fmt = spec(StageId::Formatting, Lane::Pure);
    book.keep(&fmt, &over(&fmt, &gui()), 5 * GIB)
        .expect("nothing to keep");
    assert_eq!(book.records().rows().len(), 1);
    let nowhere = Book::at(None, Some(24 * GIB), None);
    assert_eq!((book.budget(), nowhere.budget()), (21 * GIB, u64::MAX));
    nowhere
        .keep(&compile(), &over(&compile(), &gui()), 5 * GIB)
        .expect("nowhere");
    assert_eq!(
        nowhere.need(&compile(), &over(&compile(), &gui())).how,
        How::Cold
    );
}

/// How many times [`counted_reader`] was asked, by pid.
static READS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn counted_reader(pid: u32) -> Option<crate::unitgraph::UnitName> {
    READS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(pid);
    (pid != 3).then(|| crate::unitgraph::UnitName {
        package: "aterm-gui".to_owned(),
        kind: if pid == 1 {
            "aterm_gui test"
        } else {
            "aterm_gui lib"
        }
        .to_owned(),
    })
}

/// A METER THAT ATTRIBUTES READS EACH HEAVY COMPILER ONCE: a member is read for its unit the first
/// time its footprint reaches the floor (1 GiB), and never again however often it is booked; its
/// unit's peak is the most it held, its lifetime peak included; a light member is never read; a
/// member that compiles no unit (pid 3) is read once and books nothing; two compilers of one unit
/// keep the most of them. A command line names its unit by `--crate-name`, `--test` or
/// `--crate-type`, and `CARGO_PKG_NAME`. NEGATIVE CONTROL: with the floor ignored by hand, the
/// light member was read.
#[test]
fn a_meter_reads_each_heavy_compiler_once_for_its_unit() {
    let m = |pid: u32, start: u64, footprint: u64, peak: u64| aterm_sysprobe::MemberUsage {
        pid,
        start,
        footprint,
        peak_footprint: peak,
        ..aterm_sysprobe::MemberUsage::default()
    };
    let meter = Meter::attributing(counted_reader, UNIT_FLOOR);
    meter.book(9, &[m(1, 5, GIB / 2, GIB / 2), m(2, 5, 2 * GIB, 2 * GIB)]);
    meter.book(
        9,
        &[
            m(1, 5, 4 * GIB, 6 * GIB),
            m(2, 5, GIB, 2 * GIB),
            m(3, 5, 5 * GIB, 5 * GIB),
        ],
    );
    meter.book(
        9,
        &[
            m(1, 5, 5 * GIB, 6 * GIB),
            m(4, 6, 3 * GIB, 3 * GIB),
            m(5, 7, GIB / 4, GIB / 4),
        ],
    );
    let peaks: Vec<(String, u64)> = meter
        .unit_peaks()
        .into_iter()
        .map(|(n, p)| (n.kind, p))
        .collect();
    assert_eq!(
        peaks,
        [
            ("aterm_gui lib".to_owned(), 3 * GIB),
            ("aterm_gui test".to_owned(), 6 * GIB)
        ]
    );
    let mut reads = READS.lock().unwrap_or_else(PoisonError::into_inner).clone();
    reads.sort_unstable();
    assert_eq!(
        reads,
        [1, 2, 3, 4],
        "each heavy member once, the light one never"
    );
    assert!(Meter::new().unit_peaks().is_empty());

    let words = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let env = words(&["HOME=/h", "CARGO_PKG_NAME=aterm-gui"]);
    assert_eq!(
        unit_of(
            words(&[
                "rustc",
                "--crate-name",
                "aterm_gui",
                "--edition=2024",
                "--test"
            ]),
            env.clone()
        ),
        Some(crate::unitgraph::UnitName {
            package: "aterm-gui".to_owned(),
            kind: "aterm_gui test".to_owned()
        })
    );
    assert_eq!(
        unit_of(
            words(&["rustc", "--crate-name", "aterm_gui", "--crate-type", "lib"]),
            env.clone()
        )
        .map(|n| n.kind),
        Some("aterm_gui lib".to_owned())
    );
    assert_eq!(unit_of(words(&["ld", "-o", "x"]), env), None);
    assert_eq!(
        unit_of(
            words(&["rustc", "--crate-name", "x", "--test"]),
            words(&["HOME=/h"])
        ),
        None
    );
}

#[test]
fn a_metering_guard_restores_this_threads_meter_in_nesting_order() {
    let outer_meter = Meter::new();
    let inner_meter = Meter::new();
    let holds = |meter: &Arc<Meter>| current().is_some_and(|current| Arc::ptr_eq(&current, meter));
    assert!(current().is_none(), "no stage runs on this thread yet");
    let outer = metering(Some(Arc::clone(&outer_meter)));
    assert!(holds(&outer_meter));
    {
        let _inner = metering(Some(Arc::clone(&inner_meter)));
        assert!(holds(&inner_meter));
        let _cleared = metering(None);
        assert!(current().is_none(), "a nested guard can clear the meter");
    }
    assert!(
        holds(&outer_meter),
        "the inner guard's drop brought the outer meter back"
    );
    // The worker pattern the guards still allow: the meter crosses threads, the
    // guard is made and dropped on the thread it restores.
    let worker_meter = Arc::clone(&inner_meter);
    std::thread::spawn(move || {
        assert!(current().is_none(), "a fresh thread starts with no meter");
        let _own = metering(Some(worker_meter));
        assert!(current().is_some());
    })
    .join()
    .expect("the worker thread");
    assert!(
        holds(&outer_meter),
        "a worker's own guard leaves this thread's meter alone"
    );
    drop(outer);
    assert!(
        current().is_none(),
        "the outer guard's drop restored what was here before it"
    );
}

#[test]
fn a_covering_guard_restores_this_threads_marker_in_nesting_order() {
    assert!(cover().is_none(), "no stage runs on this thread yet");
    let outer = covering(Some(Arc::from("outer-stage")));
    assert_eq!(cover().as_deref(), Some("outer-stage"));
    {
        let _inner = covering(Some(Arc::from("inner-stage")));
        assert_eq!(cover().as_deref(), Some("inner-stage"));
        let _cleared = covering(None);
        assert!(cover().is_none(), "a nested guard can clear the marker");
    }
    assert_eq!(
        cover().as_deref(),
        Some("outer-stage"),
        "the inner guard's drop brought the outer marker back"
    );
    // The worker pattern the guards still allow: the marker crosses threads, the
    // guard is made and dropped on the thread it restores.
    std::thread::spawn(|| {
        assert!(cover().is_none(), "a fresh thread starts with no marker");
        let _own = covering(Some(Arc::from("worker-stage")));
        assert_eq!(cover().as_deref(), Some("worker-stage"));
    })
    .join()
    .expect("the worker thread");
    assert_eq!(
        cover().as_deref(),
        Some("outer-stage"),
        "a worker's own guard leaves this thread's marker alone"
    );
    drop(outer);
    assert!(
        cover().is_none(),
        "the outer guard's drop restored what was here before it"
    );
}
