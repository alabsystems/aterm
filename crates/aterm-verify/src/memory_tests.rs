// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a stage claims from the peaks kept for it ([`super::need`]), and the record they are
//! kept in ([`super::Records`]), on fixed records: no test reads this user's own.

use super::*;
use crate::plan::{Lane, StageId, StageSpec};
use crate::scope::Scope;

const GIB: u64 = 1 << 30;

/// Recorded workspace peaks on 2026-11-09: the pooled lint must read the history
/// written by its own width. Removing the pooled lookup makes the first assertion fail;
/// borrowing it for an unseen width or an unpooled run fails the negative controls.
#[test]
fn pooled_lint_claim_reads_its_recorded_width_and_retains_the_peak_margin() {
    let lint = spec(StageId::Tippy, Lane::TippyTarget);
    let ordinary = Key {
        checkout: "/old".into(),
        ..Key::of(StageId::Tippy, &Scope::workspace())
    };
    let pooled = crate::compile_width::record_key(ordinary.clone(), Some(18));
    let mut records = Records::default();
    records.push(ordinary.clone(), 32_233_980_136, 1);
    for peak in [23_196_705_704, 24_190_650_856, 23_279_492_256] {
        records.push(pooled.clone(), peak, 2);
    }
    let key = Key {
        checkout: "/new".into(),
        ..ordinary
    };
    let claim = |jobs| {
        need_for_jobs(
            &records,
            &Records::default(),
            &lint,
            &key,
            Some(128 * GIB),
            jobs,
        )
        .bytes
    };
    assert_eq!(claim(Some(18)), aterm_capacity::peak_claim(24_190_650_856));
    assert_eq!(claim(Some(9)), aterm_capacity::peak_claim(32_233_980_136));
    assert_eq!(claim(None), aterm_capacity::peak_claim(32_233_980_136));
    #[cfg(unix)]
    {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.json");
        records.save(&path).unwrap();
        let mut ctx = crate::Ctx::new(
            dir.path().to_owned(),
            Scope::workspace(),
            crate::EnvSnapshot::default(),
            dir.path().join("scratch"),
        )
        .with_checks(&[crate::Check::Lint]);
        ctx.memory = Some(Book::at(Some(path), Some(128 * GIB), Some(120 * GIB)));
        ctx.pool = Some(
            aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 18)
                .unwrap()
                .into(),
        );
        let planned = |ctx: &crate::Ctx| {
            crate::plan::plan(ctx)
                .into_iter()
                .find(|stage| stage.id == StageId::Tippy)
                .unwrap()
                .mem
        };
        assert_eq!(
            planned(&ctx),
            claim(Some(18)),
            "the real planner reads the pooled history"
        );
        ctx.env.cargo_build_jobs = Some("9".into());
        assert_eq!(
            planned(&ctx),
            claim(Some(9)),
            "an unseen width keeps the conservative fallback"
        );
        ctx.pool = None;
        assert_eq!(
            planned(&ctx),
            claim(None),
            "unpooled work keeps its own peak"
        );
    }
    // Both fresh runs from the paired cold workload update the same width's book.
    for peak in [23_826_572_800, 26_452_845_576] {
        records.push(pooled.clone(), peak, 3);
    }
    assert_eq!(
        need_for_jobs(
            &records,
            &Records::default(),
            &lint,
            &key,
            Some(128 * GIB),
            Some(18)
        )
        .bytes,
        31_743_414_691,
    );
    // An observed heavy run of the same width must raise the claim, never be capped
    // at the historical 27 GiB figure or at the cold estimate.
    records.push(pooled, 40 * GIB, 3);
    assert_eq!(
        need_for_jobs(
            &records,
            &Records::default(),
            &lint,
            &key,
            Some(128 * GIB),
            Some(18)
        )
        .bytes,
        aterm_capacity::peak_claim(40 * GIB),
    );
}

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
    // The test run's dispatch claims what its plan holds together once it knows its binaries, so
    // the stage claims nothing here (A9). NEGATIVE CONTROL: a stage that runs only what an
    // earlier one built, one at a time (a measuring row), still claims nothing for the old reason.
    let run = spec(StageId::Test, Lane::MainTarget);
    let n = need(
        &none,
        &Records::default(),
        &run,
        &over(&run, &gui()),
        Some(24 * GIB),
    );
    assert_eq!((n.bytes, n.how), (0, How::Dispatched));
    let rows = spec(StageId::MeasuringTests, Lane::MainTarget);
    let n = need(
        &none,
        &Records::default(),
        &rows,
        &over(&rows, &gui()),
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
            variant: Some("cells linux-arm".to_owned()),
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
/// wider selection's, wins over another checkout's; its own selection's in another checkout wins
/// over another checkout's record of a wider selection (30 GiB), and another stage's or another
/// variant's (30 GiB each) is not read. NEGATIVE CONTROL: read in its own checkout alone, as
/// before, the new worktree's compile claimed the cold 12 GiB.
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
            variant: Some("cells linux-arm".to_owned()),
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

/// A SELECTION NEVER RUN, OF CRATES RUN IN NARROWER SELECTIONS, CLAIMS THEIR SUM: on a 48 GiB
/// Mac, `-p a -p b` in a new checkout, with `-p a` kept at 3 GiB in /one and `-p b` at 2 and then
/// 4 GiB in /two, claims the sum of the most each kept (7 GiB) and a fifth more, 8.4 GiB, not the
/// cold 16, and says so. Of the covers of `-p a -p b -p c`, the least: `-p a -p b`, weighed at the
/// 4 GiB `-p b` inside it kept (its own run kept 2), and `-p c` (1 GiB), over the three singles'
/// 8; parts may share a crate; a sum whose claim passes the cold figure claims the cold figure.
/// The rules before it come first: `-p a -p b`'s own record in another checkout (5 GiB) and a
/// wider selection's in its checkout (12 GiB, more than the sum). NEGATIVE CONTROL: with `-p c`
/// in no narrower record of the stage and variant (a lint's record of it is another stage's, and
/// one of another variant, its lib alone, compiles less), `-p a -p b -p c` claims the cold figure;
/// and the test run, which compiles nothing, still claims nothing over the same records.
#[test]
fn a_selection_of_crates_kept_apart_claims_their_sum() {
    let at = |checkout: &str, id: StageId, crates: &[&str]| Key {
        checkout: checkout.to_owned(),
        ..Key::of(id, &packages(crates))
    };
    let tc = StageId::TestCompile;
    let need_of = |r: &Records, crates: &[&str]| {
        need(
            r,
            &Records::default(),
            &compile(),
            &at("/new", tc, crates),
            Some(48 * GIB),
        )
    };
    let mut r = Records::default();
    r.push(at("/one", tc, &["a"]), 3 * GIB, 1);
    r.push(at("/two", tc, &["b"]), 2 * GIB, 2);
    r.push(at("/two", tc, &["b"]), 4 * GIB, 3);
    r.push(at("/one", StageId::Tippy, &["c"]), GIB, 4);
    r.push(
        Key {
            variant: Some(crate::shape::LIB_VARIANT.to_owned()),
            ..at("/one", tc, &["c"])
        },
        GIB,
        4,
    );
    let part = |key: Key, most: u64| CoverPart {
        key,
        most,
        inside: None,
    };
    let n = need_of(&r, &["a", "b"]);
    assert_eq!(
        (n.bytes, &n.how),
        (
            aterm_capacity::peak_claim(7 * GIB),
            &How::Composed(Cover {
                parts: vec![
                    part(at("/one", tc, &["a"]), 3 * GIB),
                    part(at("/two", tc, &["b"]), 4 * GIB)
                ],
                sum: 7 * GIB,
                inside: None,
                peak: 7 * GIB,
            })
        ),
        "{n}"
    );
    assert!(
        n.to_string().contains(
            "the sum of what runs of -p a (3 GiB) and -p b (4 GiB) kept (7 GiB) and a fifth \
             more, at least 1 GiB"
        ),
        "{n}"
    );
    let n = need_of(&r, &["a", "b", "c"]);
    assert_eq!((n.bytes, &n.how), (16 * GIB, &How::Cold), "{n}");

    r.push(at("/one", tc, &["c"]), GIB, 5);
    r.push(at("/two", tc, &["a", "b"]), 2 * GIB, 6);
    let n = need_of(&r, &["a", "b", "c"]);
    assert_eq!(
        (n.bytes, &n.how),
        (
            aterm_capacity::peak_claim(5 * GIB),
            &How::Composed(Cover {
                parts: vec![
                    CoverPart {
                        inside: Some(at("/two", tc, &["b"])),
                        ..part(at("/two", tc, &["a", "b"]), 4 * GIB)
                    },
                    part(at("/one", tc, &["c"]), GIB)
                ],
                sum: 5 * GIB,
                inside: None,
                peak: 5 * GIB,
            })
        ),
        "the least cover, each part weighed at least at what a selection inside it kept"
    );
    assert!(
        n.to_string()
            .contains("-p a -p b (4 GiB, kept by -p b inside it) and -p c (1 GiB) kept (5 GiB)"),
        "{n}"
    );
    let mut shared = Records::default();
    shared.push(at("/one", tc, &["a", "b"]), 2 * GIB, 1);
    shared.push(at("/two", tc, &["b", "c"]), 3 * GIB, 2);
    assert_eq!(
        need_of(&shared, &["a", "b", "c"]).bytes,
        aterm_capacity::peak_claim(5 * GIB),
        "parts that share a crate"
    );
    let mut heavy = Records::default();
    heavy.push(at("/one", tc, &["a"]), 10 * GIB, 1);
    heavy.push(at("/one", tc, &["b"]), 9 * GIB, 2);
    let n = need_of(&heavy, &["a", "b"]);
    assert_eq!(
        (n.bytes, matches!(n.how, How::Composed(_))),
        (16 * GIB, true)
    );
    assert!(n.to_string().contains("at most the cold figure"), "{n}");

    r.push(at("/two", tc, &["a", "b"]), 5 * GIB, 7);
    assert!(
        matches!(need_of(&r, &["a", "b"]).how, How::Elsewhere(_)),
        "its own record in another checkout"
    );
    r.push(at("/new", tc, &["a", "b", "c", "d"]), 12 * GIB, 8);
    assert_eq!(
        need_of(&r, &["a", "b", "c"]).bytes,
        aterm_capacity::peak_claim(12 * GIB),
        "a wider selection's in its checkout"
    );
    let run = spec(StageId::Test, Lane::MainTarget);
    let mut runs = Records::default();
    runs.push(Key::of(StageId::Test, &packages(&["a"])), 3 * GIB, 1);
    runs.push(Key::of(StageId::Test, &packages(&["b"])), 3 * GIB, 2);
    let n = need(
        &runs,
        &Records::default(),
        &run,
        &over(&run, &packages(&["a", "b"])),
        Some(48 * GIB),
    );
    // A test run is not composed from stage records: its dispatch claims from each binary's own
    // history once it knows the plan (A9).
    assert_eq!((n.bytes, n.how), (0, How::Dispatched));
}

/// A COVER WEIGHS AT LEAST WHAT SELECTIONS INSIDE IT KEPT: with the lint's `-p a -p b` kept at
/// 205 MiB (a run over a warm cache, which compiled little), `-p c` at 1 GiB and `-p a` at
/// 10 GiB, `-p a -p b -p c` in a new checkout claims `-p a -p b` at the 10 GiB `-p a` inside it
/// kept, and `-p c`: 11 GiB and a fifth more, not the warm run's 205 MiB and `-p c`'s 1 GiB and
/// the margin, and says so. With `-p b -p c` kept at 12 GiB too, a record that straddles that
/// least cover's parts, it claims those 12 GiB and a fifth more, and says why. NEGATIVE CONTROL:
/// with no run of `-p a` kept, nothing contradicts `-p a -p b`'s light record, and the same
/// selection claims what it and `-p c` kept.
#[test]
fn a_cover_weighs_at_least_what_selections_inside_it_kept() {
    let at = |checkout: &str, crates: &[&str]| Key {
        checkout: checkout.to_owned(),
        ..Key::of(StageId::Tippy, &packages(crates))
    };
    let lint = spec(StageId::Tippy, Lane::TippyTarget);
    let need_of = |r: &Records| {
        need(
            r,
            &Records::default(),
            &lint,
            &at("/new", &["a", "b", "c"]),
            Some(48 * GIB),
        )
    };
    let warm = GIB / 5;
    let mut r = Records::default();
    r.push(at("/old", &["a", "b"]), warm, 1);
    r.push(at("/old", &["c"]), GIB, 2);
    let n = need_of(&r);
    assert_eq!(n.bytes, aterm_capacity::peak_claim(warm + GIB), "{n}");

    r.push(at("/other", &["a"]), 10 * GIB, 3);
    let weighed = vec![
        CoverPart {
            key: at("/old", &["a", "b"]),
            most: 10 * GIB,
            inside: Some(at("/other", &["a"])),
        },
        CoverPart {
            key: at("/old", &["c"]),
            most: GIB,
            inside: None,
        },
    ];
    let n = need_of(&r);
    assert_eq!(
        (n.bytes, &n.how),
        (
            aterm_capacity::peak_claim(11 * GIB),
            &How::Composed(Cover {
                parts: weighed.clone(),
                sum: 11 * GIB,
                inside: None,
                peak: 11 * GIB,
            })
        ),
        "{n}"
    );
    assert!(
        n.to_string().contains(
            "the sum of what runs of -p a -p b (10 GiB, kept by -p a inside it) and -p c (1 GiB) \
             kept (11 GiB)"
        ),
        "{n}"
    );

    r.push(at("/other", &["b", "c"]), 12 * GIB, 4);
    let n = need_of(&r);
    assert_eq!(
        (n.bytes, &n.how),
        (
            aterm_capacity::peak_claim(12 * GIB),
            &How::Composed(Cover {
                parts: weighed,
                sum: 11 * GIB,
                inside: Some((at("/other", &["b", "c"]), 12 * GIB)),
                peak: 12 * GIB,
            })
        ),
        "a record that straddles the parts"
    );
    assert!(
        n.to_string().contains(
            "the most a run of -p b -p c inside it kept (12 GiB), more than the sum of what runs \
             of -p a -p b (10 GiB, kept by -p a inside it) and -p c (1 GiB) kept (11 GiB), and a \
             fifth more"
        ),
        "{n}"
    );
}

/// A NEW WORKTREE CLAIMS WHAT A WIDER SELECTION KEPT ELSEWHERE (measured 2026-11-06 on this
/// 48 GiB Mac): a fresh worktree's `verify -p aterm-sysprobe`, with no run of `-p aterm-sysprobe`
/// kept anywhere, claimed its test compile and its lint the cold 16 GiB each, and each waited 16
/// minutes for it and peaked under 1 GiB, while another checkout's record of `-p aterm-release -p
/// aterm-sysprobe` held the test compile at 4.55 GiB and the lint at 3.4. Each now claims that
/// and a fifth more ([`wider_elsewhere`]), and says so. The test run, which compiles nothing,
/// claims nothing beside a wider test run's record elsewhere: its dispatch claims for its
/// binaries ([`How::Dispatched`]). NEGATIVE CONTROL, by
/// hand: with the rule taken out of [`need`], the test compile claimed the cold 16 GiB
/// ([`How::Cold`]).
#[test]
fn a_new_worktree_claims_what_a_wider_selection_kept_elsewhere() {
    let at = |checkout: &str, id: StageId, crates: &[&str]| Key {
        checkout: checkout.to_owned(),
        ..Key::of(id, &packages(crates))
    };
    let wide = ["aterm-release", "aterm-sysprobe"];
    let compile_peak = 4_885_525_299; // 4.55 GiB.
    let lint_peak = 3_650_722_201; // 3.4 GiB.
    let mut r = Records::default();
    r.push(
        at("/sw-trust", StageId::TestCompile, &wide),
        compile_peak,
        1,
    );
    r.push(at("/sw-trust", StageId::Tippy, &wide), lint_peak, 2);
    for (stage, peak) in [
        (compile(), compile_peak),
        (spec(StageId::Tippy, Lane::TippyTarget), lint_peak),
    ] {
        let n = need(
            &r,
            &Records::default(),
            &stage,
            &at("/vwider", stage.id, &["aterm-sysprobe"]),
            Some(48 * GIB),
        );
        assert_eq!(
            (n.bytes, &n.how),
            (
                aterm_capacity::peak_claim(peak),
                &How::WiderElsewhere(Wider {
                    key: at("/sw-trust", stage.id, &wide),
                    peak,
                    runs: 1,
                })
            ),
            "{n}"
        );
        assert!(
            n.to_string().contains(&format!(
                "the most a run over a wider selection (-p aterm-release -p aterm-sysprobe) in \
                 /sw-trust used ({}) and a fifth more, at least 1 GiB: no run of it is kept",
                aterm_capacity::size(peak)
            )),
            "{n}"
        );
    }
    let run = spec(StageId::Test, Lane::MainTarget);
    r.push(at("/sw-trust", StageId::Test, &wide), 2 * GIB, 3);
    let n = need(
        &r,
        &Records::default(),
        &run,
        &at("/vwider", StageId::Test, &["aterm-sysprobe"]),
        Some(48 * GIB),
    );
    assert_eq!((n.bytes, n.how), (0, How::Dispatched));
}

/// A WIDER RECORD ELSEWHERE CLAIMS AT MOST THE COLD FIGURE: `-p a` in a new checkout, with `-p a
/// -p b` kept at 20 GiB in /far, claims 16 GiB on a 48 GiB Mac and 12 on a 24 GiB one, the cold
/// figure, and says so: the rule stands in for the cold figure and only ever lowers it. The most
/// of every wider record elsewhere answers, so `-p a -p c` kept at 2 GiB in /near beside it
/// lowers nothing: a light superset is as likely a warm run as a tighter bound. NEGATIVE CONTROL,
/// by hand: with the cap taken out of [`need`], the 48 GiB Mac's claim was 24 GiB.
#[test]
fn a_wider_record_elsewhere_claims_at_most_the_cold_figure() {
    let at = |checkout: &str, crates: &[&str]| Key {
        checkout: checkout.to_owned(),
        ..over(&compile(), &packages(crates))
    };
    let mut r = Records::default();
    r.push(at("/far", &["a", "b"]), 20 * GIB, 1);
    r.push(at("/near", &["a", "c"]), 2 * GIB, 2);
    for (physical, want) in [(48 * GIB, 16 * GIB), (24 * GIB, 12 * GIB)] {
        let n = need(
            &r,
            &Records::default(),
            &compile(),
            &at("/new", &["a"]),
            Some(physical),
        );
        assert_eq!(n.bytes, want, "{n}");
        assert!(matches!(n.how, How::WiderElsewhere(_)), "{n}");
        assert!(
            n.to_string().contains(
                "(20 GiB) and a fifth more, at most the cold figure (16 GiB, at most half of \
                 physical memory)"
            ),
            "{n}"
        );
    }
}

/// THE LESSER OF A WIDER RECORD ELSEWHERE AND A COVER: `-p a -p b` in a new checkout, with `-p a`
/// kept at 3 GiB in /one and `-p b` at 4 GiB in /two (a cover of 7 GiB, claiming 8.4) and `-p a
/// -p b -p c` at 5 GiB in /far, claims the wider record's 6 GiB; with the wider record at 9 GiB
/// instead, the cover's 8.4; at 7 GiB, a tie, the cover's. A wider record lighter than a
/// selection inside the key kept (2 GiB, a warm run, beside `-p b`'s 4) bounds nothing: the
/// cover answers (8.4 GiB), and with no cover (no `-p a` kept) the cold figure, since `-p b`'s
/// record bounds none of `a`; at `-p b`'s 4 GiB, no lighter, it answers (5 GiB). NEGATIVE
/// CONTROLS, by hand: the wider record read before the cover claimed 10.8 GiB for the second; the
/// cover read before the wider record claimed 8.4 GiB for the first; with the warm record let in
/// ([`Records::wider_elsewhere`] without its narrower records' refusal), it claimed 3 GiB beside
/// the cover.
#[test]
fn a_selection_claims_the_lesser_of_a_wider_record_elsewhere_and_a_cover() {
    let at = |checkout: &str, crates: &[&str]| Key {
        checkout: checkout.to_owned(),
        ..over(&compile(), &packages(crates))
    };
    let need_of = |r: &Records| {
        need(
            r,
            &Records::default(),
            &compile(),
            &at("/new", &["a", "b"]),
            Some(48 * GIB),
        )
    };
    let mut parts = Records::default();
    parts.push(at("/one", &["a"]), 3 * GIB, 1);
    parts.push(at("/two", &["b"]), 4 * GIB, 2);
    let with_wider = |base: &Records, gib: u64| {
        let mut r = base.clone();
        r.push(at("/far", &["a", "b", "c"]), gib * GIB, 3);
        r
    };
    let n = need_of(&with_wider(&parts, 5));
    assert_eq!(
        (n.bytes, matches!(n.how, How::WiderElsewhere(_))),
        (aterm_capacity::peak_claim(5 * GIB), true),
        "{n}"
    );
    for gib in [9, 7] {
        let n = need_of(&with_wider(&parts, gib));
        assert_eq!(
            (n.bytes, matches!(n.how, How::Composed(_))),
            (aterm_capacity::peak_claim(7 * GIB), true),
            "{gib} GiB: {n}"
        );
    }
    let mut lone = Records::default();
    lone.push(at("/two", &["b"]), 4 * GIB, 2);
    for (base, want, cover) in [
        (&parts, aterm_capacity::peak_claim(7 * GIB), true),
        (&lone, 16 * GIB, false),
    ] {
        let n = need_of(&with_wider(base, 2));
        assert_eq!(
            (
                n.bytes,
                matches!(n.how, How::Composed(_)),
                n.how == How::Cold
            ),
            (want, cover, !cover),
            "{n}"
        );
    }
    let n = need_of(&with_wider(&lone, 4));
    assert_eq!(
        (n.bytes, matches!(n.how, How::WiderElsewhere(_))),
        (aterm_capacity::peak_claim(4 * GIB), true),
        "{n}"
    );
}

/// A WIDER RECORD ELSEWHERE LIGHTER THAN A UNIT OF THE TEST COMPILE'S CRATES IS REFUSED: `-p a`,
/// one crate, which no narrower named selection is inside, in /new, with `-p a -p b` kept at
/// 2 GiB in /far and `a`'s lib-test unit kept at 5 GiB in /old (a cold compile of `-p a` compiles
/// that unit, so the wider run that kept 2 GiB was warm): its test compile claims the cold
/// figure, and so does a `-p a@1.0.0` spec's, read by its package name. A wider record at the
/// unit's 5 GiB answers. The lint, whose work keeps no unit's peak, still reads its own wider
/// record elsewhere however warm (the documented limit), and a shaped call's variant reads no
/// floor: it compiles part of the work. NEGATIVE CONTROL, by hand: with the floor not passed
/// ([`wider_elsewhere`] reading `records.wider_elsewhere(key, 0)`), the first test compile
/// claimed 3 GiB, the wider record's 2 GiB and 1 GiB more.
#[test]
fn a_wider_record_elsewhere_lighter_than_a_unit_of_the_test_compile_is_refused() {
    assert_eq!(
        Key::of(StageId::TestCompile, &gui()).stage,
        aterm_capacity::book::TEST_COMPILE,
        "the book's test compile is verify's"
    );
    let at = |checkout: &str, id: StageId, crates: &[&str]| Key {
        checkout: checkout.to_owned(),
        ..Key::of(id, &packages(crates))
    };
    let unit = crate::shape::unit_key(
        "/old",
        &crate::unitgraph::UnitName {
            package: "a".to_owned(),
            kind: "a lib".to_owned(),
        },
    );
    let with_wider = |gib: u64| {
        let mut r = Records::default();
        r.push(unit.clone(), 5 * GIB, 1);
        for id in [StageId::TestCompile, StageId::Tippy] {
            r.push(at("/far", id, &["a", "b"]), gib * GIB, 2);
        }
        r
    };
    let warm = with_wider(2);
    let need_of = |r: &Records, stage: &StageSpec, key: &Key| {
        need(r, &Records::default(), stage, key, Some(48 * GIB))
    };
    for crates in [["a"], ["a@1.0.0"]] {
        let n = need_of(
            &warm,
            &compile(),
            &at("/new", StageId::TestCompile, &crates),
        );
        assert_eq!((n.bytes, &n.how), (16 * GIB, &How::Cold), "{crates:?}: {n}");
    }
    let n = need_of(
        &with_wider(5),
        &compile(),
        &at("/new", StageId::TestCompile, &["a"]),
    );
    assert_eq!(
        (n.bytes, matches!(n.how, How::WiderElsewhere(_))),
        (aterm_capacity::peak_claim(5 * GIB), true),
        "{n}"
    );
    let lint = spec(StageId::Tippy, Lane::TippyTarget);
    let n = need_of(&warm, &lint, &at("/new", StageId::Tippy, &["a"]));
    assert_eq!(
        (n.bytes, matches!(n.how, How::WiderElsewhere(_))),
        (aterm_capacity::peak_claim(2 * GIB), true),
        "{n}"
    );
    let lib = Key {
        variant: Some("lib".to_owned()),
        ..at("/new", StageId::TestCompile, &["a"])
    };
    assert_eq!(warm.unit_floor(&lib), 0, "a shaped call's variant");
    assert_eq!(
        warm.unit_floor(&at("/new", StageId::TestCompile, &["a"])),
        5 * GIB
    );
}

/// A run keys each stage's record by its checkout, its selection where the stage reads it, and
/// its variant ([`StageKey::of_run`]): the test compile over `-p aterm-gui` in `/ck` by that crate
/// in `/ck`; the export, whose work is the same whatever is selected, by the workspace; the test
/// run by nothing more; the cells asked for by their sorted names, and cells a selection pulled
/// by its crates. NEGATIVE CONTROL: the same stages in another checkout
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
    assert_eq!(Key::of_run(StageId::Test, &here).variant, None);
    let checker = Key::of_run(StageId::SpecCheckers, &here);
    assert_eq!(checker.variant, None);
    assert_eq!(checker.crates, Some(vec!["aterm-gui".to_owned()]));
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
/// every run: the measuring rows' light peak after a heavy one. The test run, whose dispatch
/// claims from each binary's own history, keeps none. NEGATIVE CONTROL: with the compile kept as
/// the measuring rows are, the 2 GiB run was kept.
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
    let run = spec(StageId::MeasuringTests, Lane::MainTarget);
    let run_key = over(&run, &gui());
    assert_eq!(
        book.keep(&run, &run_key, 16 * GIB).expect("kept"),
        Kept::Kept
    );
    assert_eq!(book.keep(&run, &run_key, GIB).expect("kept"), Kept::Kept);
    assert_eq!(book.records().learned(&run_key).map(|l| l.runs), Some(2));
    let tests = spec(StageId::Test, Lane::MainTarget);
    let tests_key = over(&tests, &gui());
    assert_eq!(
        book.keep(&tests, &tests_key, 16 * GIB).expect("nothing"),
        Kept::Nowhere
    );
    assert_eq!(book.records().learned(&tests_key).map(|l| l.runs), None);
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
        book.need(&compile(), &over(&compile(), &gui()), None).how,
        How::Cold
    );
    book.keep(&compile(), &over(&compile(), &gui()), 5 * GIB)
        .expect("kept");
    assert_eq!(
        book.need(&compile(), &over(&compile(), &gui()), None).bytes,
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
        nowhere
            .need(&compile(), &over(&compile(), &gui()), None)
            .how,
        How::Cold
    );
}

/// How many times [`counted_reader`] was asked, by pid.
static READS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Pid 3 compiles no unit; pid 6 reads as nothing once (a read between Cargo's fork and its
/// exec) and then as `aterm_grid lib`; pid 8 reads as an `aterm_gui lib` compiler once (a linker
/// that compiler started, read before its exec) and then as nothing; pid 9 reads as
/// `aterm_term lib`'s admission wrapper once (the compiler that wrapper started, read before its
/// exec) and then as that compiler; pid 4 is an `aterm_gui lib` compiler unreadable after its
/// first read; pid 7 is `aterm_gui lib`'s admission wrapper; pid 5 compiles `aterm_cell lib`;
/// pid 1 `aterm_gui test`; every other pid `aterm_gui lib`.
fn counted_reader(pid: u32) -> Named {
    let earlier = {
        let mut reads = READS.lock().unwrap_or_else(PoisonError::into_inner);
        reads.push(pid);
        reads.iter().filter(|&&p| p == pid).count() - 1
    };
    let (package, kind, wrapper) = match (pid, earlier) {
        (3, _) | (6, 0) | (8, 1..) => return Named::Nothing,
        (4, 1..) => return Named::Unread,
        (1, _) => ("aterm-gui", "aterm_gui test", false),
        (5, _) => ("aterm-cell", "aterm_cell lib", false),
        (6, _) => ("aterm-grid", "aterm_grid lib", false),
        (9, e) => ("aterm-term", "aterm_term lib", e == 0),
        (7, _) => ("aterm-gui", "aterm_gui lib", true),
        _ => ("aterm-gui", "aterm_gui lib", false),
    };
    Named::Unit(UnitRead {
        unit: crate::unitgraph::UnitName {
            package: package.to_owned(),
            kind: kind.to_owned(),
        },
        wrapper,
        codegen_requested: None,
    })
}

/// A METER THAT ATTRIBUTES READS EVERY MEMBER AT EVERY READ, TAKES IT FOR WHAT ITS LAST READABLE
/// READ NAMED, KEEPS ONLY HEAVY COMPILERS IN THE SHAPING PEAKS, AND RETAINS LIGHT IDENTITIES FOR
/// CLAIMS: a unit's peak is the most its compiler held, its lifetime peak included; a light unit
/// is absent from `unit_peaks` but present at the 1 GiB floor in `unit_claim_peaks`; two compilers
/// of one unit keep the most of them; a read that cannot read a member leaves its name. NEGATIVE
/// CONTROLS: dropping the light identity makes the claim map assertion fail; the wrapper (pid 7)
/// holds more than its unit's compilers, yet its unit's peak is theirs; the linker misnamed at
/// its first read (pid 8) holds 5 GiB, yet `aterm_gui lib` keeps its compilers' 3 GiB, where a
/// first name kept for good would give it 5; the compiler misnamed as its wrapper at its first
/// read (pid 9) keeps its 2 GiB peak, where that name kept would keep none; the member that
/// compiles nothing (pid 3) holds 5 GiB and keeps no peak.
#[test]
fn a_meter_names_each_member_by_its_last_readable_read_and_keeps_heavy_peaks() {
    let m = |pid: u32, start: u64, footprint: u64, peak: u64| aterm_sysprobe::MemberUsage {
        pid,
        start,
        footprint,
        peak_footprint: peak,
        ..aterm_sysprobe::MemberUsage::default()
    };
    let meter = Meter::attributing(counted_reader, UNIT_FLOOR);
    meter.book(
        9,
        &[
            m(1, 5, GIB / 2, GIB / 2),
            m(2, 5, 2 * GIB, 2 * GIB),
            m(7, 5, 4 * GIB, 4 * GIB),
            m(6, 5, GIB / 8, GIB / 8),
            m(3, 5, GIB / 8, GIB / 8),
            m(8, 5, GIB / 8, GIB / 8),
            m(9, 5, GIB / 8, GIB / 8),
        ],
    );
    meter.book(
        9,
        &[
            m(1, 5, 4 * GIB, 6 * GIB),
            m(2, 5, GIB, 2 * GIB),
            m(3, 5, 5 * GIB, 5 * GIB),
            m(6, 5, 2 * GIB, 2 * GIB),
            m(8, 5, 5 * GIB, 5 * GIB),
            m(9, 5, 2 * GIB, 2 * GIB),
        ],
    );
    for _ in 0..3 {
        meter.book(
            9,
            &[
                m(1, 5, 5 * GIB, 6 * GIB),
                m(4, 6, 3 * GIB, 3 * GIB),
                m(5, 7, GIB / 4, GIB / 4),
                m(3, 5, GIB / 8, GIB / 8),
            ],
        );
    }
    let peaks: Vec<(String, u64)> = meter
        .unit_peaks()
        .into_iter()
        .map(|(n, p)| (n.kind, p))
        .collect();
    assert_eq!(
        peaks,
        [
            ("aterm_grid lib".to_owned(), 2 * GIB),
            ("aterm_gui lib".to_owned(), 3 * GIB),
            ("aterm_gui test".to_owned(), 6 * GIB),
            ("aterm_term lib".to_owned(), 2 * GIB),
        ]
    );
    let claim_peaks: Vec<(String, u64)> = meter
        .unit_claim_peaks()
        .into_iter()
        .map(|(n, p)| (n.kind, p))
        .collect();
    assert_eq!(
        claim_peaks,
        [
            ("aterm_cell lib".to_owned(), UNIT_FLOOR),
            ("aterm_grid lib".to_owned(), 2 * GIB),
            ("aterm_gui lib".to_owned(), 3 * GIB),
            ("aterm_gui test".to_owned(), 6 * GIB),
            ("aterm_term lib".to_owned(), 2 * GIB),
        ],
        "NEGATIVE CONTROL: the claim book needs the meter-proved light identity"
    );
    let mut reads = READS.lock().unwrap_or_else(PoisonError::into_inner).clone();
    reads.sort_unstable();
    assert_eq!(
        reads,
        [
            1, 1, 1, 1, 1, 2, 2, 3, 3, 3, 3, 3, 4, 4, 4, 5, 5, 5, 6, 6, 7, 8, 8, 9, 9
        ],
        "every member at every read that found it"
    );
    assert!(Meter::new().unit_peaks().is_empty());
    assert!(Meter::new().unit_claim_peaks().is_empty());
}

/// A COMPILER'S SPAN IS ITS FIRST AND LAST READ, ITS UNIT'S WRAPPER THE ONE FIRST READ BEFORE
/// IT: a wrapper read at 0 s and its compiler read at 2 s, 4 s and 5 s span 2–5 s queued from
/// 0 s, with the CPU of the compiler's last read and the most it held; a second wrapper of the
/// same unit read after that compiler started is no wrapper of it. A meter that does not attribute
/// has no span. NEGATIVE CONTROLS: a window that starts after a compiler's first read leaves it
/// out; a compiler read once (pid 3, at 4 s) has no span.
#[test]
fn a_compilers_span_is_its_reads_and_its_wrapper_the_one_before_it() {
    fn reader(pid: u32) -> Named {
        Named::Unit(UnitRead {
            unit: crate::unitgraph::UnitName {
                package: "aterm-gui".to_owned(),
                kind: "aterm_gui lib".to_owned(),
            },
            wrapper: pid >= 10,
            codegen_requested: Some(pid >= 10),
        })
    }
    let m = |pid: u32, cpu_s: u64, footprint: u64| aterm_sysprobe::MemberUsage {
        pid,
        start: 1,
        cpu_ns: cpu_s * 1_000_000_000,
        footprint,
        ..aterm_sysprobe::MemberUsage::default()
    };
    let meter = Meter::attributing(reader, UNIT_FLOOR);
    let t0 = std::time::Instant::now();
    let at = |s: u64| t0 + std::time::Duration::from_secs(s);
    meter.book_at(1, &[m(10, 0, GIB / 16)], at(0));
    meter.book_at(1, &[m(10, 0, GIB / 16), m(2, 1, GIB)], at(2));
    meter.book_at(
        1,
        &[m(2, 3, 3 * GIB), m(11, 0, GIB / 16), m(3, 1, GIB)],
        at(4),
    );
    meter.book_at(1, &[m(2, 4, 2 * GIB)], at(5));
    let spans = meter.unit_spans(at(0), at(5));
    assert_eq!(spans.len(), 1, "{spans:?}");
    let s = &spans[0];
    assert_eq!(s.unit.kind, "aterm_gui lib");
    assert_eq!(
        s.codegen_requested,
        Some(false),
        "the compiler's output, not its wrapper's"
    );
    let queued = s.queued_at_unix_ms.expect("its wrapper was read");
    assert_eq!(s.started_at_unix_ms - queued, 2_000);
    assert_eq!(s.ended_at_unix_ms - s.started_at_unix_ms, 3_000);
    assert!((s.cpu_s - 4.0).abs() < 1e-9, "{s:?}");
    assert_eq!(s.peak_bytes, 3 * GIB);
    assert!(meter.unit_spans(at(3), at(5)).is_empty());
    assert!(Meter::new().unit_spans(at(0), at(5)).is_empty());
    assert_eq!(
        meter.unix_ms(at(5)).zip(meter.unix_ms(at(2))),
        Some((s.ended_at_unix_ms, s.started_at_unix_ms)),
        "the meter dates a window on the clock that dated its spans"
    );
    assert_eq!(Meter::new().unix_ms(at(0)), None);
}

/// A `KERN_PROCARGS2` answer of a process exec'd from `exec` with `argv` and `env`.
fn procargs(exec: &str, argv: &[&str], env: &[&str]) -> Vec<u8> {
    let mut buf = i32::try_from(argv.len()).unwrap().to_ne_bytes().to_vec();
    buf.extend(exec.as_bytes());
    buf.extend([0, 0, 0]);
    for word in argv.iter().chain(env) {
        buf.extend(word.as_bytes());
        buf.push(0);
    }
    buf.push(0);
    buf
}

/// THE ADMISSION WRAPPER IS A PROCESS EXEC'D FROM THIS VERY EXECUTABLE, naming its unit as the
/// compiler it starts does. NEGATIVE CONTROLS: a compiler whose environment carries the
/// wrapper's marker (a probe a build script runs, which inherits it from Cargo) is no wrapper;
/// with this executable unknown nothing is one; a command line naming no unit names nothing.
#[test]
fn the_wrapper_is_the_process_running_this_executable() {
    let compile = ["rustc", "--crate-name", "aterm_gui", "--crate-type", "lib"];
    let env = ["CARGO_PKG_NAME=aterm-gui", "ATERM_VERIFY_UNIT_WRAPPER=1"];
    let read = |exec: &str, argv: &[&str], own: Option<&str>| {
        let buf = procargs(exec, argv, &env);
        let args = aterm_sysprobe::procargs::parse(&buf).expect("parses");
        named(&args, own.map(Path::new))
    };
    let unit = |wrapper| {
        Named::Unit(UnitRead {
            unit: crate::unitgraph::UnitName {
                package: "aterm-gui".to_owned(),
                kind: "aterm_gui lib".to_owned(),
            },
            wrapper,
            codegen_requested: None,
        })
    };
    let mut wrapped = vec!["/bin/verify"];
    wrapped.extend(compile);
    assert_eq!(
        read("/bin/verify", &wrapped, Some("/bin/verify")),
        unit(true)
    );
    assert_eq!(
        read("/tc/rustc", &compile, Some("/bin/verify")),
        unit(false)
    );
    assert_eq!(read("/bin/verify", &wrapped, None), unit(false));
    assert_eq!(
        read("/bin/cargo", &["cargo", "build"], Some("/bin/verify")),
        Named::Nothing
    );
}

/// A host library and a checked target library have the same unit name. Their explicit
/// output requests distinguish them; missing or hidden requests remain unknown.
#[test]
fn compiler_output_separates_host_codegen_from_checked_metadata() {
    for (args, expected) in [
        (vec!["--emit=dep-info,metadata"], Some(false)),
        (vec!["--emit", "metadata=/tmp/meta"], Some(false)),
        (vec!["--emit=mir"], Some(false)),
        (vec!["--emit=dep-info,metadata,link"], Some(true)),
        (
            vec!["--emit=metadata", "--emit", "obj=/tmp/object"],
            Some(true),
        ),
        (vec!["--emit=llvm-ir"], Some(true)),
        (vec!["--emit=llvm-bc"], Some(true)),
        (vec!["--emit=asm"], Some(true)),
        (vec!["--emit=metadata", "--", "--emit=link"], Some(false)),
        (vec!["--emit=metadata", "@flags"], None),
        (vec!["--emit=unknown"], None),
        (vec!["--emit="], None),
        (vec!["--emit"], None),
        (vec!["-Clink-arg=something"], None),
        (vec![], None),
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(compiler_codegen_requested(&args), expected, "{args:?}");
    }
    let read = |emit| {
        let buf = procargs(
            "/bin/tippy-driver",
            &[
                "tippy-driver",
                "--crate-name",
                "dep",
                "--crate-type",
                "lib",
                emit,
            ],
            &["CARGO_PKG_NAME=dep"],
        );
        let args = aterm_sysprobe::procargs::parse(&buf).expect("process arguments");
        let Named::Unit(read) = named(&args, None) else {
            panic!("a compiler unit")
        };
        read
    };
    let metadata = read("--emit=dep-info,metadata");
    let linked = read("--emit=dep-info,metadata,link");
    assert_eq!(metadata.unit, linked.unit);
    assert_eq!(metadata.codegen_requested, Some(false));
    assert_eq!(linked.codegen_requested, Some(true));
}

/// A COMMAND LINE NAMES ITS UNIT by `--crate-name`, `--test` or `--crate-type`, and
/// `CARGO_PKG_NAME`. NEGATIVE CONTROLS: a command with no crate name, and a compiler whose
/// environment names no package, name none.
#[test]
fn a_command_line_names_its_unit() {
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
