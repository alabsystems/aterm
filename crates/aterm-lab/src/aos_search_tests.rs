// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

fn usage_of(args: &[&str]) -> String {
    let f = parse_args(&words(args)).expect_err("a usage error");
    assert_eq!(f.code, 2, "{:?}", f.msg);
    f.msg.unwrap_or_default()
}

#[test]
fn aos_search_parses_its_proposers_and_refuses_a_run_without_one_or_an_out() {
    assert_eq!(
        parse_args(&words(&["--grid", "--out", "/tmp/x"])).expect("parses"),
        Args {
            grid: true,
            plans: None,
            out: "/tmp/x".into(),
            toolchain: lab_build().into(),
            threads: None,
            frames: false,
        }
    );
    assert_eq!(
        parse_args(&words(&[
            "--out",
            "o",
            "--plans",
            "p.txt",
            "--grid",
            "--toolchain",
            "targo 1.99.0-dev (targo 0.11.0)",
        ]))
        .expect("parses"),
        Args {
            grid: true,
            plans: Some("p.txt".into()),
            out: "o".into(),
            toolchain: "targo 1.99.0-dev (targo 0.11.0)".into(),
            threads: None,
            frames: false,
        }
    );
    assert!(usage_of(&["--out", "o"]).contains("name a proposer"));
    assert!(usage_of(&["--grid"]).contains("--out DIR is required"));
    assert!(usage_of(&["--grid", "--out"]).contains("--out needs a value"));
    assert!(usage_of(&["--plans"]).contains("--plans needs a value"));
    assert!(usage_of(&["--grid", "--out", "o", "--toolchain", " "]).contains("non-empty"));
    assert!(usage_of(&["--grid", "--out", "o", "extra"]).contains("unexpected 'extra'"));
    assert!(lab_build().starts_with("aterm-lab+g"));
}

#[test]
fn a_small_plans_search_writes_a_verified_bundle_and_its_summary() {
    let dir = aterm_tempfile::tempdir().expect("an exclusive out dir");
    let text = "\
# the grid winner {SettleAtStep{0}, NextStep}, the cadence plan, a cold-only plan
010100000000000000000000
010003000000000000000000
010001000000000000000000
010100000000000000000000  # again: a duplicate
not a plan
";
    let args = Args {
        grid: false,
        plans: Some("plans.txt".into()),
        out: dir.path().display().to_string(),
        toolchain: "targo test-line".into(),
        threads: None,
        frames: false,
    };
    let (code, summary) = run_in(&args, Some(text), dir.path(), TEST_BUILD).expect("runs");
    assert_eq!(code, 0, "{summary}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("summary.txt")).expect("written"),
        summary
    );
    for want in [
        "search proposer=plans file v1 config=",
        "  counts proposed=5 distinct=4 admitted=2 oversized=0 duplicates=1 over_budget=0 rounds=4\n",
        "NoStepSlept=1",
        "PlanRefusal=1",
        " unreported=0\n",
        "  winner plan=010100000000000000000000 digest=",
        " round=0 index=0 useful=true\n",
        "  selection provenance=declared\n",
        "  held_out T=5119 Z=161 baseline T=6685 Z=1750 gain_ppm=234255\n",
        "  bundle file=plans.bundle bytes=",
        " verified=true build=aterm-lab+g",
        " toolchain=targo_test-line\n",
        "  MEASURED (never scored) wall_ms=",
        " stepped=",
        "aos-search result=pass searches=1 winners=1\n",
    ] {
        assert!(summary.contains(want), "{want:?} in:\n{summary}");
    }
    let bytes = std::fs::read(dir.path().join("plans.bundle")).expect("the bundle");
    let ev = Evaluator::v1(TEST_BUILD, Instant::now()).expect("the pinned policy");
    let bundle = CandidateBundle::verify(&bytes, &ev).expect("the bundle verifies");
    assert_eq!(bundle.selection_provenance().name(), "declared");
    let parts = bundle.parts();
    assert_eq!(parts.toolchain, "targo test-line");
    let labels: Vec<&str> = parts.timings.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(labels, [SEARCH_TIMING, EVALUATOR_TIMING]);
    assert!(
        parts.publication.is_none(),
        "a lab search publishes nothing"
    );
    let (version, commit) = TEST_BUILD
        .rsplit_once("+g")
        .expect("TEST_BUILD names its commit");
    let first = if commit.starts_with('0') { '1' } else { '0' };
    let other = format!("{version}+g{first}{}", commit.get(1..).unwrap_or_default());
    let ev = Evaluator::v1(&other, Instant::now()).expect("the pinned policy");
    assert_eq!(
        CandidateBundle::verify(&bytes, &ev),
        Err(BundleRefusal::Forged {
            field: BundleField::Build,
            row: None
        }),
        "{other}"
    );

    let none = aterm_tempfile::tempdir().expect("an exclusive out dir");
    let (code, summary) = run_in(
        &args,
        Some("010001000000000000000000\n"),
        none.path(),
        TEST_BUILD,
    )
    .expect("runs");
    assert_eq!(code, 1, "{summary}");
    assert!(
        summary.contains("  no-outcome no outcome: none of 1 distinct"),
        "{summary}"
    );
    assert!(summary.contains("aos-search result=fail searches=1 winners=0\n"));
    assert!(!none.path().join("plans.bundle").exists());
}

const TEST_BUILD: &str = "aterm-lab+g0123456789ab";

const GRID_STATS: SearchStats = SearchStats {
    rounds: 4,
    proposed: 54,
    oversized: 0,
    duplicates: 0,
    over_budget: 0,
    distinct: 54,
    admitted: 18,
};

fn evaluator() -> &'static Evaluator {
    static EV: std::sync::OnceLock<Evaluator> = std::sync::OnceLock::new();
    EV.get_or_init(|| Evaluator::v1(TEST_BUILD, Instant::now()).expect("the pinned policy"))
}

fn searched_bundle() -> Vec<u8> {
    static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BYTES
        .get_or_init(|| {
            let dir = aterm_tempfile::tempdir().expect("an exclusive out dir");
            let args = Args {
                grid: false,
                plans: Some("plans.txt".into()),
                out: dir.path().display().to_string(),
                toolchain: "targo test-line".into(),
                threads: None,
                frames: false,
            };
            let (code, summary) = run_in(
                &args,
                Some("010100000000000000000000\n"),
                dir.path(),
                TEST_BUILD,
            )
            .expect("runs");
            assert_eq!(code, 0, "{summary}");
            std::fs::read(dir.path().join("plans.bundle")).expect("the bundle it wrote")
        })
        .clone()
}

fn report(bytes: &[u8], lab: &str) -> (i32, String) {
    verify_bundle(bytes, "t.bundle", lab, || {
        Ok::<_, PolicyRefusal>(evaluator())
    })
}

fn report_unbuilt(bytes: &[u8], lab: &str) -> (i32, String) {
    verify_bundle(
        bytes,
        "t.bundle",
        lab,
        || -> Result<&Evaluator, PolicyRefusal> {
            panic!("an evaluator was built for a bundle refused before one")
        },
    )
}

fn resealed(bytes: &[u8], edit: impl FnOnce(&mut UncheckedBundle)) -> Vec<u8> {
    let mut b = UncheckedBundle::decode(bytes).expect("the bundle decodes");
    edit(&mut b);
    b.encode()
}

fn statuses(report: &str) -> Vec<(String, String)> {
    report
        .lines()
        .filter_map(|l| l.strip_prefix("  check "))
        .map(|l| {
            let head = l.split(' ').next().unwrap_or_default();
            let (name, status) = head.split_once('=').unwrap_or((head, ""));
            (name.to_owned(), status.to_owned())
        })
        .collect()
}

fn refused_at(at: Check, grid: &str) -> Vec<(String, String)> {
    Check::ALL
        .into_iter()
        .map(|c| {
            let status = match c.cmp(&at) {
                std::cmp::Ordering::Less if c == Check::Grid => grid,
                std::cmp::Ordering::Less => "pass",
                std::cmp::Ordering::Equal => "fail",
                std::cmp::Ordering::Greater => "not-run",
            };
            (c.name().to_owned(), status.to_owned())
        })
        .collect()
}

fn verdict(report: &str) -> &str {
    report.lines().last().unwrap_or_default()
}

#[test]
fn the_verify_flag_takes_one_file_and_nothing_else() {
    let verify_usage = |args: &[&str]| {
        let f = parse_verify(&words(args)).expect_err("a usage error");
        assert_eq!(f.code, 2, "{:?}", f.msg);
        f.msg.unwrap_or_default()
    };
    assert_eq!(
        parse_verify(&words(&["--verify", "b.bundle"])).expect("parses"),
        Some("b.bundle".to_owned())
    );
    assert_eq!(
        parse_verify(&words(&["--grid", "--out", "o"])).expect("parses"),
        None
    );
    assert!(verify_usage(&["--verify"]).contains("--verify needs a value"));
    assert!(verify_usage(&["--verify", "b", "--grid"]).contains("unexpected '--grid'"));
    assert!(verify_usage(&["--out", "o", "--verify", "b"]).contains("unexpected '--out'"));
    assert!(verify_usage(&["--verify", "a", "--verify", "b"]).contains("unexpected '--verify'"));
    assert!(Check::ALL.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(
        word("a b\n\\\u{e9}=train-03"),
        "a_b\\u{a}\\\\\\u{e9}=train-03"
    );
}

#[test]
fn a_bundle_a_small_search_wrote_verifies_offline_one_line_per_check() {
    let bytes = searched_bundle();
    let (code, r) = report(&bytes, TEST_BUILD);
    assert_eq!(code, 0, "{r}");
    let mut want = refused_at(Check::Publication, "declared");
    want[10].1 = "none".to_owned();
    assert_eq!(statuses(&r), want, "{r}");
    for line in [
        "aos-search verify file=t.bundle bytes=",
        &format!(" lab={}\n", word(TEST_BUILD)),
        &format!("  check build=pass build={}\n", word(TEST_BUILD)),
        "  check plan=pass plan=010100000000000000000000 candidate=momentum-glow.schedule-plan ",
        "  check proposer=pass mode=search proposer=file v1 config=",
        " round=0 index=0\n",
        "  check rows=pass rows=26,",
        " proposed=1 distinct=1 admitted=1 oversized=0 duplicates=0 over_budget=0 rounds=4 \
         held_out_looks=1 ",
        " held_out T=5119 P=4958 Z=161 baseline T=6685 P=4935 Z=1750 ",
        " useful=true gain_ppm=234255 s0_gain_ppm=99423\n",
        "  check grid=declared provenance=declared (credited to file v1, not the grid",
        "  check timings=pass labelled=2 ",
        "  check publication=none ",
        "  MEASURED (never scored) evaluator_build_ms=",
    ] {
        assert!(r.contains(line), "{line:?} in:\n{r}");
    }
    assert_eq!(verdict(&r), "aos-search verify=pass reason=none");
    assert!(r.contains(&format!(
        "  check decode=pass digest={} format=1\n",
        Digest::of(BUNDLE_DOMAIN, &[&bytes]).short()
    )));
}

#[test]
fn a_cut_a_flipped_and_a_resealed_bundle_are_refused_naming_the_reason() {
    let bytes = searched_bundle();
    let n = bytes.len();
    let flipped = |at: usize, bit: u8| {
        let mut b = bytes.clone();
        b[at] ^= bit;
        b
    };
    for (forged, reason) in [
        (bytes[..n - 1].to_vec(), "Seal"),
        (bytes[..n / 2].to_vec(), "Seal"),
        (bytes[..20].to_vec(), "Wire{Truncated,field=bundle.seal}"),
        (bytes[..5].to_vec(), "Magic"),
        (Vec::new(), "Magic"),
        (flipped(n / 2, 0x10), "Seal"),
        (flipped(n - 1, 0x01), "Seal"),
        (flipped(0, 0x01), "Magic"),
        (flipped(8, 0x02), "Format{found=3}"),
    ] {
        let (code, r) = report_unbuilt(&forged, TEST_BUILD);
        assert_eq!(code, 1, "{r}");
        assert_eq!(statuses(&r), refused_at(Check::Decode, "pass"), "{r}");
        assert_eq!(
            verdict(&r),
            format!("aos-search verify=fail reason={reason}")
        );
        assert!(!r.contains("MEASURED"), "{r}");
    }

    let item = UncheckedBundle::decode(&bytes).expect("decodes").rows[3]
        .item
        .clone();
    assert_eq!(item, "train-03");
    let more_ticks = resealed(&bytes, |b| b.rows[3].ticks += 1);
    let (code, r) = report(&more_ticks, TEST_BUILD);
    assert_eq!(code, 1, "{r}");
    assert_eq!(statuses(&r), refused_at(Check::Rows, "declared"), "{r}");
    assert!(r.contains(
        "  check rows=fail Forged{field=Ticks,row=train-03}: the bundle is forged: the T of \
         the row for train-03 is not what the pinned policy computes\n"
    ));
    assert_eq!(
        verdict(&r),
        "aos-search verify=fail reason=Forged{field=Ticks,row=train-03}"
    );
    assert!(r.contains("  MEASURED (never scored) evaluator_build_ms="));

    for (edit, at, reason) in [
        (
            (|b: &mut UncheckedBundle| b.round = MAX_ROUNDS) as fn(&mut UncheckedBundle),
            Check::Proposer,
            "Forged{field=Proposer}",
        ),
        (
            |b: &mut UncheckedBundle| b.selection.stats.proposed += 1,
            Check::Selection,
            "Forged{field=Stats}",
        ),
        (
            |b: &mut UncheckedBundle| b.selection.train.candidate.ticks += 1,
            Check::Selection,
            "Forged{field=Sums}",
        ),
        (
            |b: &mut UncheckedBundle| b.timings[0].label.clear(),
            Check::Timings,
            "Forged{field=Timings}",
        ),
        (
            |b: &mut UncheckedBundle| b.plan[0] = 2,
            Check::Plan,
            "Forged{field=Plan}",
        ),
        (
            |b: &mut UncheckedBundle| b.grade = "Proved".into(),
            Check::Identities,
            "Forged{field=Grade}",
        ),
        (
            |b: &mut UncheckedBundle| {
                b.plan[0] = 2;
                b.grade = "Proved".into();
            },
            Check::Identities,
            "Forged{field=Grade}",
        ),
    ] {
        let (code, r) = report(&resealed(&bytes, edit), TEST_BUILD);
        assert_eq!(code, 1, "{r}");
        assert_eq!(statuses(&r), refused_at(at, "declared"), "{reason}:\n{r}");
        assert_eq!(
            verdict(&r),
            format!("aos-search verify=fail reason={reason}")
        );
    }
}

#[test]
fn a_bundle_of_another_build_is_refused_before_any_evaluator_is_built() {
    let bytes = searched_bundle();
    let v = "0.157.0";
    let other = "aterm-lab+g0123456789ac".to_owned();
    let (code, r) = report_unbuilt(&bytes, &other);
    assert_eq!(code, 1, "{r}");
    assert_eq!(statuses(&r), refused_at(Check::Build, "pass"), "{r}");
    assert_eq!(verdict(&r), "aos-search verify=fail reason=ForeignBuild");
    assert!(r.contains(&format!(
        "  check build=fail ForeignBuild: the bundle names the build {}, not this lab's {}:",
        word(TEST_BUILD),
        word(&other)
    )));
    let window = resealed(&bytes, |b| b.build = format!("{v}+g0123456789ab"));
    let (code, r) = report_unbuilt(&window, TEST_BUILD);
    assert_eq!(
        (code, verdict(&r)),
        (1, "aos-search verify=fail reason=ForeignBuild")
    );

    for (build, reason) in [
        (format!("{TEST_BUILD}-dirty"), "DirtyBuild"),
        ("aterm-lab+gunknown".to_owned(), "UnstampedBuild"),
        ("aterm-lab".to_owned(), "UnstampedBuild"),
    ] {
        let named = resealed(&bytes, |b| b.build = build.clone());
        let (code, r) = report_unbuilt(&named, &build);
        assert_eq!(code, 1, "{r}");
        assert_eq!(statuses(&r), refused_at(Check::Build, "pass"), "{r}");
        assert_eq!(
            verdict(&r),
            format!("aos-search verify=fail reason={reason}")
        );
    }

    let hostile = resealed(&bytes, |b| {
        b.build = "x\naos-search verify=pass reason=none\n  check build=pass".into();
    });
    let (code, r) = report_unbuilt(&hostile, TEST_BUILD);
    assert_eq!(code, 1, "{r}");
    assert_eq!(r.lines().count(), 1 + Check::ALL.len() + 1, "{r}");
    assert_eq!(
        r.lines()
            .filter(|l| l.starts_with("aos-search verify="))
            .count(),
        1,
        "{r}"
    );
    assert_eq!(verdict(&r), "aos-search verify=fail reason=ForeignBuild");
    for (file, shown) in [
        (
            "x\naos-search verify=pass reason=none\n.bundle",
            "file=x\\u{a}aos-search_verify=pass_reason=none\\u{a}.bundle ",
        ),
        ("My Bundles/b.bundle", "file=My_Bundles/b.bundle "),
    ] {
        let (code, r) = verify_bundle(
            &bytes,
            file,
            &other,
            || -> Result<&Evaluator, PolicyRefusal> {
                panic!("an evaluator was built for a bundle refused before one")
            },
        );
        assert_eq!(code, 1, "{r}");
        assert_eq!(r.lines().count(), 1 + Check::ALL.len() + 1, "{r}");
        assert_eq!(
            r.lines()
                .filter(|l| l.starts_with("aos-search verify="))
                .count(),
            1,
            "{r}"
        );
        assert!(
            r.lines()
                .next()
                .is_some_and(|l| l.starts_with(&format!("aos-search verify {shown}"))),
            "{r}"
        );
        assert_eq!(verdict(&r), "aos-search verify=fail reason=ForeignBuild");
    }

    let (code, r) = verify_bundle(&bytes, "t.bundle", TEST_BUILD, || {
        Err::<&Evaluator, _>(PolicyRefusal::CorpusDrift {
            item: "golden".into(),
        })
    });
    assert_eq!(code, 1, "{r}");
    assert_eq!(statuses(&r), refused_at(Check::Evaluator, "pass"), "{r}");
    assert_eq!(
        verdict(&r),
        "aos-search verify=fail reason=CorpusDrift{item=golden}"
    );

    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let file = dir.path().join("plans.bundle");
    std::fs::write(&file, &bytes).expect("written");
    let path = file.display().to_string();
    assert_eq!(
        cmd_aos_search(&words(&["--verify", &path])).expect("a report"),
        1,
        "a bundle of TEST_BUILD is foreign to this lab's build {}",
        lab_build()
    );
    let missing = dir.path().join("none.bundle").display().to_string();
    let f = cmd_aos_search(&words(&["--verify", &missing])).expect_err("usage");
    assert_eq!(f.code, 2, "{:?}", f.msg);
}

#[test]
fn a_published_bundle_of_this_labs_build_passes_the_publication_check() {
    use aterm_effects::aos_momentum::plan::{SchedulePlan, Split, Wake};
    use aterm_effects::aos_momentum::{MomentumCode, MomentumHost};
    let origin = Instant::now();
    let winner =
        SchedulePlan::new(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep).expect("in range");
    let admitted = evaluator()
        .admit_one(&winner.encode())
        .expect("the winner is admitted");
    let mut host = MomentumHost::default();
    host.aos_attach(origin, MomentumCode::Shipped, TEST_BUILD, 1);
    host.aos_pause(origin, Duration::from_nanos(16_666_667))
        .expect("a running glow pauses");
    let published = host.aos_publish(admitted).expect("the winner publishes");
    let bytes = published.bundle().encode();
    let (code, r) = report(&bytes, TEST_BUILD);
    assert_eq!(code, 0, "{r}");
    let mut want = refused_at(Check::Publication, "declared");
    want[10].1 = "pass".to_owned();
    assert_eq!(statuses(&r), want, "{r}");
    assert!(
        r.contains(&format!(
            "  check publication=pass revision={} live=momentum-glow.shipped \
             candidate=momentum-glow.schedule-plan ",
            published.revision().0
        )),
        "{r}"
    );
    assert_eq!(verdict(&r), "aos-search verify=pass reason=none");
}

#[test]
fn the_verify_file_is_read_bounded_and_only_when_it_is_a_regular_file() {
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let sized = |name: &str, len: u64| {
        let path = dir.path().join(name);
        std::fs::File::create(&path)
            .and_then(|f| f.set_len(len))
            .expect("a sparse file");
        path.display().to_string()
    };
    let usage = |path: &str| {
        let f = cmd_aos_search(&words(&["--verify", path])).expect_err("a usage error");
        assert_eq!(f.code, 2, "{:?}", f.msg);
        f.msg.unwrap_or_default()
    };
    let over = sized("over.bundle", MAX_BUNDLE_BYTES + 1);
    assert!(
        usage(&over).ends_with(&format!(": holds more than {MAX_BUNDLE_BYTES} bytes")),
        "{over}"
    );
    let at = sized("at.bundle", MAX_BUNDLE_BYTES);
    assert_eq!(
        cmd_aos_search(&words(&["--verify", &at])).expect("a report"),
        1,
        "read whole, then refused at decode"
    );
    for path in [dir.path().display().to_string(), "/dev/zero".to_owned()] {
        assert!(usage(&path).ends_with(": not a regular file"), "{path}");
    }
    assert_eq!(
        read_bounded(&at, MAX_BUNDLE_BYTES).map(|b| b.len()),
        Ok(usize::try_from(MAX_BUNDLE_BYTES).expect("16 MiB"))
    );
    assert_eq!(
        read_bounded(&over, MAX_BUNDLE_BYTES),
        Err(format!("holds more than {MAX_BUNDLE_BYTES} bytes"))
    );
}

/// A selection credited to the grid is refused at the step that checks it. Two refusals that
/// fall before the grid's own re-run run end to end. The grid's own refusals and a good
/// grid-credited bundle's lines are read directly off the mapping ([`verify_step`]) and the
/// report ([`passed`]), since each end-to-end run re-runs the whole grid: that `verify` refuses
/// a selection the grid would not make is `bundle_tests`'
/// `a_selection_credited_to_the_grid_is_the_grids`. No test in this crate runs a grid-credited
/// bundle through `--verify` to a pass, and this mapping would go stale unseen if `verify`
/// checked the grid before the selection.
#[test]
fn a_selection_credited_to_the_grid_is_refused_at_the_step_that_checks_it() {
    let bytes = searched_bundle();
    let grid = |b: &mut UncheckedBundle| b.proposer = GridProposer::new().id();
    for (edit, at, reason) in [
        (
            (|b: &mut UncheckedBundle| b.round = MAX_ROUNDS) as fn(&mut UncheckedBundle),
            Check::Proposer,
            "Forged{field=Proposer}",
        ),
        (
            |b: &mut UncheckedBundle| {
                b.index = 6;
                b.selection.stats = SearchStats {
                    rounds: 3,
                    ..GRID_STATS
                };
            },
            Check::Selection,
            "Forged{field=Stats}",
        ),
    ] {
        let forged = resealed(&bytes, |b| {
            grid(b);
            edit(b);
        });
        let (code, r) = report(&forged, TEST_BUILD);
        assert_eq!(code, 1, "{r}");
        assert_eq!(statuses(&r), refused_at(at, "pass"), "{reason}:\n{r}");
        assert_eq!(
            verdict(&r),
            format!("aos-search verify=fail reason={reason}")
        );
    }

    let forged = |field| BundleRefusal::Forged { field, row: None };
    let mut credited = UncheckedBundle::decode(&bytes).expect("the bundle decodes");
    grid(&mut credited);
    assert_eq!(
        verify_step(&forged(BundleField::Proposer), &credited),
        Check::Grid,
        "another plan credited to the grid"
    );
    credited.index = 6;
    credited.selection.stats = SearchStats {
        admitted: 17,
        ..GRID_STATS
    };
    assert_eq!(
        verify_step(&forged(BundleField::Stats), &credited),
        Check::Grid,
        "counts that hold but are not the grid's"
    );
    credited.selection.stats = GRID_STATS;
    let (status, line) = passed(Check::Grid, &credited, &bytes, "");
    assert_eq!(status, "pass", "{line}");
    assert!(line.starts_with("provenance=rederived "), "{line}");
    let (status, line) = passed(Check::Proposer, &credited, &bytes, "");
    assert_eq!(status, "pass", "{line}");
    assert!(line.contains(" proposer=grid v1 "), "{line}");
}

/// `--threads` takes 1 to 256, and `--frames`, which says a run's steps, parses in a debug build
/// with or without it.
#[test]
fn threads_are_bounded_and_frames_parses_with_or_without_threads() {
    let a = parse_args(&words(&["--grid", "--out", "o", "--threads", "4"])).expect("parses");
    assert_eq!((a.threads, a.frames), (Some(4), false));
    for bad in ["0", "257", "two", "+1"] {
        assert!(
            usage_of(&["--grid", "--out", "o", "--threads", bad]).contains("takes 1 to 256"),
            "{bad}"
        );
    }
    if cfg!(debug_assertions) {
        for local in [
            &["--grid"][..],
            &["--plans", "p"],
            &["--grid", "--threads", "2"],
        ] {
            let a =
                parse_args(&words(&[local, &["--out", "o", "--frames"]].concat())).expect("parses");
            assert!(a.frames, "{local:?}");
        }
    }
}
