// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a stage claims from the peaks kept for it ([`super::need`]), and the record they are
//! kept in ([`super::Records`]), on fixed records: no test reads this user's own.

use super::*;
use crate::plan::{Lane, StageId, StageSpec};
use crate::scope::Scope;

const GIB: u64 = 1 << 30;

fn spec(id: StageId, lane: Lane) -> StageSpec {
    StageSpec {
        id,
        title: format!("{id:?}"),
        lane,
        exclusive: false,
        after_stages: Vec::new(),
        mem: 0,
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
        let n = need(&none, &compile(), &over(&compile(), &gui()), physical);
        assert_eq!((n.bytes, &n.how), (want, &How::Cold), "{physical:?}");
    }
    let run = spec(StageId::Test, Lane::MainTarget);
    let n = need(&none, &run, &over(&run, &gui()), Some(24 * GIB));
    assert_eq!((n.bytes, n.how), (0, How::Light));
    let fmt = spec(StageId::Formatting, Lane::Pure);
    let pure = kept(StageId::Formatting, &gui(), &[3]);
    let n = need(&pure, &fmt, &over(&fmt, &gui()), Some(24 * GIB));
    assert_eq!((n.bytes, n.how), (0, How::Unmetered));
    let warm = kept(StageId::TestCompile, &gui(), &[5]);
    assert_eq!(
        need(&warm, &compile(), &over(&compile(), &gui()), Some(24 * GIB)).bytes,
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
    let n = need(&r, &compile(), &at("/new", &gui()), Some(24 * GIB));
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
    let n = need(&wide, &compile(), &at("/new", &gui()), Some(24 * GIB));
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
    let n = need(&r, &compile(), &at("/new", &gui()), Some(24 * GIB));
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
    let mut cells = ctx("/ck");
    cells.cells = vec!["win".to_owned(), "linux-arm".to_owned()];
    let foreign = Key::of_run(StageId::ForeignCells, &cells);
    assert_eq!(
        (foreign.crates, foreign.variant),
        (None, Some("cells linux-arm win".to_owned()))
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

/// The records read back as they were written, through the file too; text that is not theirs
/// reads as none. NEGATIVE CONTROL: another schema reads as none.
#[test]
fn the_records_read_back_as_written() {
    let mut r = kept(StageId::TestCompile, &Scope::workspace(), &[3, 9]);
    r.push(Key::of(StageId::Doctests, &packages(&["z", "y"])), 5, 7);
    let text = r.encode();
    assert_eq!(Records::decode(&text), r);
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("state/verify/stage-memory.json");
    r.save(&path).expect("saved");
    assert_eq!(Records::load(&path), r);
    assert_eq!(
        std::fs::read_dir(path.parent().expect("its folder"))
            .expect("listed")
            .count(),
        1,
        "no stage folder is left beside it"
    );
    assert_eq!(Records::decode("not json"), Records::default());
    assert_eq!(
        Records::load(&dir.path().join("absent")),
        Records::default()
    );
    let other = text.replacen("\"schema\":2", "\"schema\":1", 1);
    assert_ne!(other, text);
    assert_eq!(Records::decode(&other), Records::default());
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

/// Every stage that runs a build of its own claims the cold figure with nothing kept: the spec
/// checker (`targo run -p aterm-models`), the gpu oracle (`targo test -p aterm-gpu`), the
/// codex-alab proof (the aterm it drives) and the Linux lane (the lab it boots its VM with) among
/// them. The stages that run only what an earlier one built claim nothing. NEGATIVE CONTROL: the
/// list before the review left those four out, so each claimed nothing.
#[test]
fn every_stage_that_builds_claims_the_cold_figure() {
    for id in [
        StageId::SpecCheck,
        StageId::GpuOracle,
        StageId::CodexAlabProof,
        StageId::LinuxLane,
    ] {
        assert!(compiles(id), "{id:?}");
    }
    for id in [
        StageId::Test,
        StageId::SpecCheckers,
        StageId::MeasuringTests,
    ] {
        assert!(!compiles(id), "{id:?}");
    }
}
