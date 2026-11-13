// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::EnvSnapshot;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

/// Removing the production Census call to the structural guard removes this gate failure.
#[test]
fn census_refuses_an_entry_a_branch_adds_outside_a_released_section() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let env = EnvSnapshot {
        path: std::env::var_os("PATH").unwrap_or_default(),
        ..Default::default()
    };
    let git = |args: &[&str]| {
        let out = crate::changed::git(root, &env.path)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "A"]);
    git(&["config", "user.email", "a@example.invalid"]);
    let kept = "## [Unreleased]\n\n### Fixed\n\n- Kept note.\n";
    std::fs::write(root.join("CHANGELOG.md"), kept).unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    git(&["branch", "base"]);
    std::fs::create_dir_all(root.join(CHECKOUT_MARKER).parent().unwrap()).unwrap();
    std::fs::write(root.join(CHECKOUT_MARKER), "").unwrap();
    let ctx = Ctx::new(
        root.to_path_buf(),
        Scope::changed("base", vec![], false),
        env.clone(),
        root.join("scratch"),
    );
    for stray in [
        format!("- Stray entry.\n\n{kept}"),
        format!("{kept}\n- Another note.\n"),
    ] {
        std::fs::write(root.join("CHANGELOG.md"), stray).unwrap();
        let mut report = Report::new("census");
        census(&ctx, &mut report);
        assert!(report.outcomes().any(|(outcome, label)| outcome
            == Outcome::Fail(Severity::GateFailed)
            && label == "changelog structure"));
    }
    std::fs::write(root.join("CHANGELOG.md"), kept).unwrap();
    std::fs::write(root.join("unrelated.txt"), "work").unwrap();
    let mut report = Report::new("census");
    census(&ctx, &mut report);
    assert!(
        !report
            .outcomes()
            .any(|(_, label)| label == "changelog structure")
    );
}

#[test]
fn census_reports_harness_debt_as_a_gate_failure() {
    let temp = aterm_tempfile::TempDir::new().expect("scratch repository");
    let root = temp.path();
    let git = |args: &[&str]| {
        let out = crate::git_provider::command()
            .current_dir(root)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "A"]);
    git(&["config", "user.email", "a@example.invalid"]);
    let version_file = aterm_phase::CliHarness::ClaudeCode.version_file();
    let source = "crates/aterm-agent/src/harness/upgrade_drive.rs";
    for path in [version_file, source, CHECKOUT_MARKER] {
        std::fs::create_dir_all(root.join(path).parent().expect("parent")).expect("mkdir");
    }
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\n[workspace.package]\nversion = \"0.1.0\"\n",
    )
    .expect("manifest");
    std::fs::write(
        root.join(version_file),
        format!("{}\n", aterm_phase::harness_version::source_line((0, 1))),
    )
    .expect("version source");
    std::fs::write(root.join(source), "fn drive() {}\n").expect("harness source");
    std::fs::write(root.join(CHECKOUT_MARKER), "").expect("checkout marker");
    git(&["add", "."]);
    git(&["commit", "-qm", "public release"]);
    git(&["tag", "-am", "public release", "v0.1.0"]);
    std::fs::write(root.join(source), "fn drive() { changed(); }\n")
        .expect("changed harness source");
    git(&["add", "."]);
    git(&["commit", "-qm", "unbumped harness source"]);

    let ctx = Ctx::new(
        root.to_path_buf(),
        Scope::workspace(),
        EnvSnapshot {
            // This fixture runs genuine Git, including its normal user-prefix launcher.
            path: std::env::var_os("PATH").unwrap_or_default(),
            ..EnvSnapshot::default()
        },
        root.join("scratch"),
    );
    let mut report = Report::new("census");
    census(&ctx, &mut report);
    let rows: Vec<_> = report.outcomes().collect();
    // NEGATIVE CONTROL: removing Census's harness-debt call leaves no debt-labelled gate
    // failure even though the source changed after the public tag.
    assert!(
        rows.iter().any(|(outcome, label)| {
            *outcome == Outcome::Fail(Severity::GateFailed)
                && label.contains("harness landing debt since public v0.1.0")
        }),
        "production Census stage did not report the debt: {rows:?}"
    );
}

/// THE CENSUS WALK HOLDS CAPTURES TO THE BASELINE OF THE ENGINE'S COMMIT: a capture holding a
/// word only the named engine's committed baseline lists is a `capture-privacy` finding of
/// the guard census. NEGATIVE CONTROL: the same word left uncommitted in that engine's
/// working tree (what the census read before) judges nothing, and the walk says it held
/// captures to the commit's baseline.
#[test]
fn the_census_walk_holds_captures_to_the_engines_committed_baseline() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let write = |path: &std::path::Path, text: &str| {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    };
    let git = |at: &std::path::Path, args: &[&str]| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let out = crate::changed::git(at, &path)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    let root = dir.path().join("repo");
    write(
        &root.join(aterm_census::export_scan::rows::EXTRA),
        "# x\nzebrafish\n",
    );
    write(&root.join("crates/x/src/lib.rs"), "// x\n");
    let capture: String = b"plain quokkaword\r\n"
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    write(
        &root.join("crates/x/tests/fixtures/base.ptylog"),
        &format!("1000 O {capture}\n"),
    );
    let engine = dir.path().join("engine");
    let baseline = engine.join(aterm_census::export_scan::rows::BASELINE);
    write(&baseline, "# rows\n");
    git(dir.path(), &["init", "-q", "-b", "main", "engine"]);
    git(&engine, &["add", "-A"]);
    git(&engine, &["commit", "-q", "-m", "engine"]);
    write(&baseline, "# rows\nquokkaword\n");
    let named = engine.clone().into_os_string();
    let var = move |name: &str| (name == "PUBLICATION_ENGINE").then(|| named.clone());
    let guards: Vec<&aterm_census::Census> = aterm_census::CENSUSES
        .iter()
        .filter(|c| c.name == "guards")
        .collect();
    assert_eq!(guards.len(), 1, "the guard census");
    let log = |outcomes: Vec<aterm_census::CensusOutcome>| {
        outcomes.into_iter().map(|o| o.log).collect::<String>()
    };
    let uncommitted = log(census_walk(&root, &var, &guards, None).0);
    assert!(
        !uncommitted.contains("base.ptylog"),
        "NEGATIVE CONTROL: an uncommitted row is no part of the engine: {uncommitted}"
    );
    git(&engine, &["commit", "-q", "-am", "row"]);
    let committed = log(census_walk(&root, &var, &guards, None).0);
    assert!(
        committed.contains("base.ptylog"),
        "the committed row holds: {committed}"
    );
}

/// A cone lints all the targets of what the change touched and the default targets of the rest,
/// matching by package name whichever spec the cone holds it by. CONTROL: `-p`, `--all`, an
/// empty cone, a cone where everything is touched (a lock row's) stay one `--all-targets` pass;
/// and a list that matches nothing never reaches [`Scope::args`], which would answer
/// `--workspace`.
#[test]
fn a_cone_lints_all_the_targets_of_what_the_change_touched() {
    let spec = "path+file:///r/crates/aterm-x#x@1.0.0";
    let crates = vec![
        "aterm-gui".to_string(),
        "aterm-uds".to_string(),
        "getrandom@0.2.17".to_string(),
        spec.to_string(),
    ];
    let cone = |touched: &[&str]| {
        Scope::changed("main", crates.clone(), true)
            .with_all_targets(touched.iter().map(ToString::to_string).collect())
    };
    let tail = |all: bool| -> Vec<String> {
        let tail: &[&str] = if all {
            &LINT_TAIL
        } else {
            &["--keep-going", "--", "-D", "warnings"]
        };
        tail.iter().map(ToString::to_string).collect()
    };
    let select = |names: &[&str]| -> Vec<String> {
        names
            .iter()
            .flat_map(|name| ["-p".to_string(), (*name).to_string()])
            .collect()
    };
    let args = |scope: &Scope| -> Vec<Vec<String>> {
        tippy_passes(scope).into_iter().map(|p| p.args).collect()
    };
    assert_eq!(
        args(&cone(&["aterm-uds", "getrandom", "x"])),
        [
            [select(&["aterm-uds", "getrandom@0.2.17", spec]), tail(true)].concat(),
            [select(&["aterm-gui"]), tail(false)].concat(),
        ]
    );
    let passes = tippy_passes(&cone(&["aterm-uds"]));
    assert_eq!(passes[0].label, "-p aterm-uds --all-targets");
    assert!(
        passes[1]
            .label
            .starts_with("-p aterm-gui -p getrandom@0.2.17")
    );
    assert!(!passes[1].label.contains("--all-targets"), "{passes:?}");
    // What the cone holds only by a dev edge is in neither pass, matched by package name.
    let left_out = |names: &[&str]| {
        cone(&["aterm-uds"]).with_unlinted(names.iter().map(ToString::to_string).collect())
    };
    assert_eq!(
        args(&left_out(&["aterm-gui", "getrandom"])),
        [
            [select(&["aterm-uds"]), tail(true)].concat(),
            [select(&[spec]), tail(false)].concat(),
        ]
    );
    assert_eq!(
        args(&left_out(&["aterm-gui", "getrandom", "x"])),
        [[select(&["aterm-uds"]), tail(true)].concat()],
        "with none of the rest left, no second pass"
    );
    // The whole cone: a lock row the change reached.
    let whole = Scope::changed("main", crates.clone(), true);
    assert_eq!(args(&whole), [tippy_args(&whole)]);
    for scope in [Scope::workspace(), Scope::crate_only("aterm-gui")] {
        assert_eq!(args(&scope), [tippy_args(&scope)], "{scope:?}");
    }
    let empty = Scope::changed("main", Vec::new(), false);
    assert!(empty.selects_nothing());
    assert_eq!(args(&empty), [tippy_args(&empty)]);
    assert_eq!(
        args(&cone(&[])),
        [[
            select(&["aterm-gui", "aterm-uds", "getrandom@0.2.17", spec]),
            tail(false)
        ]
        .concat()],
        "no list that matches nothing widens to --workspace"
    );
}

#[test]
fn a_versioned_package_keeps_its_required_feature_lint_pass() {
    let gated = vec![("aterm-gui".to_string(), "control-conformance".to_string())];
    let bare = tippy_gated_args(&Scope::crate_only("aterm-gui"), &gated)
        .expect("the selected bare package has a required-feature pass");
    for wrong in [
        "aterm-gui-extra@0.106.0",
        "other-aterm-gui@0.106.0",
        "aterm-gui@",
    ] {
        assert_eq!(tippy_gated_args(&Scope::crate_only(wrong), &gated), None);
    }
    let spec = "aterm-gui@0.106.0";
    let scopes = [
        Scope::crate_only(spec),
        Scope::changed("main", vec![spec.into()], true),
    ];
    let mut expected = vec!["-p".to_string(), spec.to_string()];
    expected.extend(LINT_TAIL.map(String::from));
    for scope in &scopes {
        assert_eq!(
            tippy_args(scope),
            expected,
            "the ordinary pass keeps its exact version"
        );
    }
    let passes: Vec<_> = scopes
        .iter()
        .map(|scope| tippy_gated_args(scope, &gated))
        .collect();
    let mut versioned = bare;
    versioned[1] = spec.to_string();
    assert_eq!(passes, vec![Some(versioned.clone()), Some(versioned)]);
}

fn ctx(scope: Scope) -> Ctx {
    Ctx::new(
        PathBuf::from("/repo"),
        scope,
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
}

type PreparedLint = Result<(Vec<Cmd>, crate::lint_content::Evidence), String>;

struct FakeLintOps {
    batch_outcome: LintPackageOutcome,
    batch_seconds: f64,
    halt_on_batch: bool,
    halted: bool,
    prepared_boundaries: Vec<(String, LintReadBoundary)>,
    prepared: BTreeMap<String, VecDeque<PreparedLint>>,
    bounded: BTreeMap<String, (LintPackageOutcome, f64)>,
    batches: Vec<Vec<String>>,
    ran: Vec<(String, Vec<Vec<String>>)>,
    cannot_run: Vec<String>,
    all_carried: Vec<usize>,
    finished: Vec<LintPackageFinished>,
}

impl FakeLintOps {
    fn passing() -> Self {
        Self {
            batch_outcome: LintPackageOutcome::Ok,
            batch_seconds: 0.0,
            halt_on_batch: false,
            halted: false,
            prepared_boundaries: Vec::new(),
            prepared: BTreeMap::new(),
            bounded: BTreeMap::new(),
            batches: Vec::new(),
            ran: Vec::new(),
            cannot_run: Vec::new(),
            all_carried: Vec::new(),
            finished: Vec::new(),
        }
    }

    fn prepare_as(
        &mut self,
        spec: &str,
        sequence: impl IntoIterator<Item = Result<(Vec<Cmd>, crate::lint_content::Evidence), String>>,
    ) {
        self.prepared
            .insert(spec.to_owned(), sequence.into_iter().collect());
    }
}

impl LintScheduleOps for FakeLintOps {
    fn run_unbounded(
        &mut self,
        packages: &[crate::lint_content::Package],
    ) -> (LintPackageOutcome, f64) {
        self.batches.push(
            packages
                .iter()
                .map(|package| package.spec.clone())
                .collect(),
        );
        if self.halt_on_batch && self.batch_outcome != LintPackageOutcome::Ok {
            self.halted = true;
        }
        (self.batch_outcome, self.batch_seconds)
    }

    fn prepare(
        &mut self,
        package: &crate::lint_content::Package,
        boundary: LintReadBoundary,
    ) -> Result<(Vec<Cmd>, crate::lint_content::Evidence), String> {
        self.prepared_boundaries
            .push((package.spec.clone(), boundary));
        if let Some(next) = self
            .prepared
            .get_mut(&package.spec)
            .and_then(VecDeque::pop_front)
        {
            return next;
        }
        Ok((package.commands.clone(), package.evidence.clone()))
    }

    fn run_bounded(
        &mut self,
        package: &crate::lint_content::Package,
        commands: &[Cmd],
    ) -> (LintPackageOutcome, f64) {
        let argv = commands
            .iter()
            .map(|command| {
                std::iter::once(command.program.to_string_lossy().into_owned())
                    .chain(
                        command
                            .args
                            .iter()
                            .map(|arg| arg.to_string_lossy().into_owned()),
                    )
                    .collect()
            })
            .collect();
        self.ran.push((package.spec.clone(), argv));
        self.bounded
            .get(&package.spec)
            .copied()
            .unwrap_or((LintPackageOutcome::Ok, 0.0))
    }

    fn halted(&self) -> bool {
        self.halted
    }

    fn cannot_run(&mut self, why: String) {
        self.cannot_run.push(why);
    }

    fn all_carried(&mut self, count: usize) {
        self.all_carried.push(count);
    }

    fn finish(&mut self, row: &LintPackageFinished) {
        self.finished.push(row.clone());
    }
}

fn lint_evidence(byte: u8) -> crate::lint_content::Evidence {
    crate::lint_content::Evidence {
        content_key: Some(format!("{byte:02x}").repeat(32)),
        reason: None,
    }
}

fn lint_package(spec: &str, byte: u8) -> crate::lint_content::Package {
    crate::lint_content::Package {
        spec: spec.into(),
        dir: format!("crates/{spec}"),
        evidence: lint_evidence(byte),
        commands: vec![Cmd::new("/fixture/tippy").args([
            "--unverified",
            "tippy",
            "-p",
            spec,
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])],
    }
}

fn unbounded_lint_package(spec: &str) -> crate::lint_content::Package {
    crate::lint_content::Package {
        spec: spec.into(),
        dir: format!("crates/{spec}"),
        evidence: unbounded_lint_evidence("an undeclared build or proc-macro reader"),
        commands: Vec::new(),
    }
}

fn lint_proof(
    package: &crate::lint_content::Package,
    execution_s: f64,
) -> crate::lint_content::CarryProof {
    crate::lint_content::CarryProof::passed(&package.spec, &package.evidence, execution_s)
}

/// Real prepare's snapshot adapter receives the same package identity and exact command list
/// the scheduler will dispatch. Mutating either field, dropping a command, or returning a
/// different snapshot result fails this control.
#[test]
fn real_lint_prepare_binds_snapshot_evidence_to_its_exact_package() {
    let package = lint_package("identity@1", 1);
    let expected = lint_evidence(99);
    let observed = std::cell::RefCell::new(None);
    let evidence =
        read_lint_package_evidence(&package, &package.commands, |spec, dir, commands| {
            *observed.borrow_mut() = Some((spec.to_owned(), dir.to_owned(), commands.to_vec()));
            expected.clone()
        });
    assert_eq!(
        observed.into_inner(),
        Some((
            package.spec.clone(),
            package.dir.clone(),
            package.commands.clone()
        ))
    );
    assert_eq!(evidence, expected);
}

/// The all-hit path still rehashes each package, but starts no Tippy child. Removing the
/// carried partition or dispatching it through `run_bounded` fails this control.
#[test]
fn an_all_carried_lint_starts_no_tippy_child() {
    let package = lint_package("carried@1", 1);
    let selection = crate::lint_content::Selection {
        run: Vec::new(),
        carried: vec![(package.clone(), lint_proof(&package, 7.0))],
    };
    let mut ops = FakeLintOps::passing();
    schedule_lint_packages(selection, &mut ops);

    assert!(ops.batches.is_empty(), "no unbounded Tippy batch");
    assert!(ops.ran.is_empty(), "no per-package Tippy child");
    assert_eq!(ops.all_carried, [1]);
    assert_eq!(
        ops.prepared_boundaries,
        [("carried@1".into(), LintReadBoundary::Final)],
        "a carry is read only at the fresh final boundary"
    );
    assert_eq!(ops.finished.len(), 1);
    assert!(ops.finished[0].carried);
    assert_eq!(ops.finished[0].outcome, "ok");
}

/// The production command constructor supplies the scheduler's exact argv, and only the miss
/// is dispatched. Mutating the scheduler to run the carried package adds its spec to `ran`;
/// mutating the constructor loses the exact `-p <spec>` command asserted here.
#[test]
fn a_lint_partition_dispatches_only_misses_with_the_exact_package_argv() {
    let command_ctx = ctx(Scope::workspace());
    let mut missed = lint_package("missed@1", 2);
    let fresh_commands = exact_package_tippy_commands(
        &command_ctx,
        std::path::Path::new("/fixture/tippy"),
        &missed.spec,
    )
    .expect("exact package command");
    assert!(
        fresh_commands
            .iter()
            .all(|command| { command.exact_env && command.runtime_environment_is_finalized() })
    );
    // The planning command is deliberately stale. Dispatch must use the command rebuilt by
    // the immediate pre-dispatch read, not this value carried in the selection.
    missed.commands = vec![Cmd::new("/stale/planning-tippy").args([
        "--unverified",
        "tippy",
        "-p",
        "wrong-planning-package@1",
    ])];
    let fresh_argv: Vec<Vec<String>> = fresh_commands
        .iter()
        .map(|command| {
            std::iter::once(command.program.to_string_lossy().into_owned())
                .chain(
                    command
                        .args
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned()),
                )
                .collect()
        })
        .collect();
    let carried = lint_package("carried@1", 3);
    let selection = crate::lint_content::Selection {
        run: vec![missed.clone()],
        carried: vec![(carried.clone(), lint_proof(&carried, 3.0))],
    };
    let mut ops = FakeLintOps::passing();
    ops.bounded
        .insert(missed.spec.clone(), (LintPackageOutcome::Ok, 5.0));
    ops.prepare_as(
        &missed.spec,
        [
            Ok((fresh_commands.clone(), missed.evidence.clone())),
            Ok((fresh_commands, missed.evidence.clone())),
        ],
    );
    schedule_lint_packages(selection, &mut ops);

    assert_eq!(ops.ran.len(), 1);
    assert_eq!(ops.ran[0].0, missed.spec);
    assert_eq!(
        ops.prepared_boundaries,
        [
            ("missed@1".into(), LintReadBoundary::PreDispatch),
            ("missed@1".into(), LintReadBoundary::Final),
            ("carried@1".into(), LintReadBoundary::Final),
        ],
        "the miss is checked immediately and finally; the carry only finally"
    );
    let argv = &ops.ran[0].1;
    assert_eq!(
        argv, &fresh_argv,
        "the child receives the freshly prepared command, never the stale planning command"
    );
    assert!(
        argv.iter()
            .all(|args| { args.windows(2).any(|pair| pair == ["-p", "missed@1"]) })
    );
    assert!(
        argv.iter()
            .all(|args| !args.iter().any(|arg| arg == "carried@1"))
    );
    assert_eq!(
        ops.finished
            .iter()
            .map(|row| (row.spec.as_str(), row.carried))
            .collect::<Vec<_>>(),
        [("carried@1", true), ("missed@1", false)]
    );
}

/// One ordinary batch has no per-package attribution. If it fails, none of its packages may
/// seed an OK and the halt starts no later bounded child.
#[test]
fn a_failed_unbounded_lint_batch_seeds_no_package_ok() {
    let selection = crate::lint_content::Selection {
        run: vec![
            unbounded_lint_package("reader-a@1"),
            unbounded_lint_package("reader-b@1"),
            lint_package("later-bounded@1", 4),
        ],
        carried: Vec::new(),
    };
    let mut ops = FakeLintOps::passing();
    ops.batch_outcome = LintPackageOutcome::Fail;
    ops.batch_seconds = 4.0;
    ops.halt_on_batch = true;
    schedule_lint_packages(selection, &mut ops);

    assert_eq!(ops.batches, [["reader-a@1", "reader-b@1"]]);
    assert!(ops.ran.is_empty(), "the halt starts no bounded child");
    assert!(ops.finished.iter().all(|row| row.outcome != "ok"));
    assert_eq!(
        ops.finished
            .iter()
            .map(|row| (row.spec.as_str(), row.outcome))
            .collect::<Vec<_>>(),
        [
            ("later-bounded@1", "NOT RUN"),
            ("reader-a@1", "FAIL"),
            ("reader-b@1", "FAIL")
        ]
    );
}

/// Both stability reads fail closed. Pre-dispatch drift starts no child and tombstones later
/// work; post-run drift replaces successful and carried candidates with unbounded NOT RUN
/// rows and gives the aggregate lint a COULD NOT RUN reason.
#[test]
fn lint_input_drift_before_or_after_execution_is_a_tombstone() {
    let first = lint_package("first@1", 5);
    let later = lint_package("later@1", 6);
    let mut pre = FakeLintOps::passing();
    pre.prepare_as(
        &first.spec,
        [Ok((first.commands.clone(), lint_evidence(90)))],
    );
    schedule_lint_packages(
        crate::lint_content::Selection {
            run: vec![first.clone(), later],
            carried: Vec::new(),
        },
        &mut pre,
    );
    assert!(pre.ran.is_empty());
    assert_eq!(pre.cannot_run.len(), 1);
    assert_eq!(
        pre.prepared_boundaries,
        [("first@1".into(), LintReadBoundary::PreDispatch)]
    );
    assert!(pre.finished.iter().all(|row| {
        row.outcome == "NOT RUN" && !row.evidence.bounded() && row.evidence.reason.is_some()
    }));

    let missed = lint_package("missed@1", 7);
    let carried = lint_package("carried@1", 8);
    let mut post = FakeLintOps::passing();
    post.prepare_as(
        &missed.spec,
        [
            Ok((missed.commands.clone(), missed.evidence.clone())),
            Ok((missed.commands.clone(), lint_evidence(91))),
        ],
    );
    post.prepare_as(
        &carried.spec,
        [Ok((carried.commands.clone(), lint_evidence(92)))],
    );
    post.bounded
        .insert(missed.spec.clone(), (LintPackageOutcome::Ok, 6.0));
    schedule_lint_packages(
        crate::lint_content::Selection {
            run: vec![missed],
            carried: vec![(carried.clone(), lint_proof(&carried, 4.0))],
        },
        &mut post,
    );
    assert_eq!(post.ran.len(), 1);
    assert_eq!(post.cannot_run.len(), 1);
    assert_eq!(
        post.prepared_boundaries,
        [
            ("missed@1".into(), LintReadBoundary::PreDispatch),
            ("missed@1".into(), LintReadBoundary::Final),
            ("carried@1".into(), LintReadBoundary::Final),
        ]
    );
    assert!(post.finished.iter().all(|row| {
        row.outcome == "NOT RUN"
            && !row.carried
            && !row.evidence.bounded()
            && row.evidence.reason.is_some()
    }));
}

/// Deterministic line timeline for the package scheduler. The four fixture packages take
/// 12, 8, 4 and 2 seconds serially. A cfg(test) edit invalidates its package plus its reverse
/// dependent; a runtime-only fixture edit changes no lint key; a verifier/context change
/// changes every bounded key. The undeclared reader always runs.
#[test]
fn lint_line_timeline_runs_3_1_4_and_models_22_2_26_seconds() {
    let base = [
        lint_package("edited@1", 10),
        lint_package("dependent@1", 11),
        lint_package("independent@1", 12),
    ];
    let passed = crate::lint_content::Passed::from([
        (base[0].spec.clone(), lint_proof(&base[0], 12.0)),
        (base[1].spec.clone(), lint_proof(&base[1], 8.0)),
        (base[2].spec.clone(), lint_proof(&base[2], 4.0)),
    ]);
    let reverse = || {
        Some(BTreeMap::from([
            ("edited@1".into(), BTreeSet::from(["dependent@1".into()])),
            ("dependent@1".into(), BTreeSet::new()),
            ("independent@1".into(), BTreeSet::new()),
            ("reader@1".into(), BTreeSet::new()),
        ]))
    };
    let run = |mut packages: Vec<crate::lint_content::Package>| {
        packages.push(unbounded_lint_package("reader@1"));
        let selection = crate::lint_content::Plan::fixture(packages, reverse()).select(&passed);
        let mut ops = FakeLintOps::passing();
        ops.batch_seconds = 2.0;
        for (spec, seconds) in [
            ("edited@1", 12.0),
            ("dependent@1", 8.0),
            ("independent@1", 4.0),
        ] {
            ops.bounded
                .insert(spec.into(), (LintPackageOutcome::Ok, seconds));
        }
        schedule_lint_packages(selection, &mut ops);
        let ran = ops.finished.iter().filter(|row| !row.carried).count();
        let carried = ops.finished.iter().filter(|row| row.carried).count();
        let wall: f64 = ops
            .finished
            .iter()
            .filter(|row| !row.carried)
            .map(|row| row.execution_s)
            .sum();
        assert!(ops.finished.iter().all(|row| row.outcome == "ok"));
        (ran, carried, wall)
    };

    let cfg_test = run(vec![
        lint_package("edited@1", 20),
        base[1].clone(),
        base[2].clone(),
    ]);
    let fixture_only = run(base.to_vec());
    let context = run(vec![
        lint_package("edited@1", 30),
        lint_package("dependent@1", 31),
        lint_package("independent@1", 32),
    ]);
    assert_eq!(cfg_test, (3, 1, 22.0));
    assert_eq!(fixture_only, (1, 3, 2.0));
    assert_eq!(context, (4, 0, 26.0));
}

/// A PURE COMPILE CALL WRITES ITS COMPILE LINE (A1): its Cargo arguments get the message
/// format before their `--` (the lint driver's arguments after it are left alone), its units
/// come from its artifact messages (2 fresh, 1 compiled, 1 with no `fresh` unknown), its
/// output bytes from the 3 MiB file the compiled one names (not the 5 MiB a fresh one names,
/// nor the file the unknown one names, which is gone), and its row is decided as
/// before, on its rendered diagnostics, with no machine message in its report and what it
/// printed for a person kept. A flag already given is not given twice, and a call with no
/// `--` gets it last. NEGATIVE CONTROLS: the flag after `--` (where the lint driver would get
/// it) fails the stand-in; the call's stdout put back whole puts `{"reason"` in the report.
#[cfg(unix)]
#[test]
fn a_pure_compile_call_writes_its_line_and_keeps_cargos_messages_out_of_its_report() {
    let root = crate::mktemp_dir("atv-compile-line").expect("scratch");
    let rlib = root.join("liba.rlib");
    std::fs::write(&rlib, vec![0_u8; 3 << 20]).expect("rlib");
    std::fs::write(root.join("b.rlib"), vec![0_u8; 5 << 20]).expect("a fresh unit's rlib");
    let artifact = |name: &str, fresh: &str, file: &std::path::Path| {
        format!(
            "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"{name}\",\"kind\":[\"lib\"]}},{fresh}\"filenames\":[\"{}\"]}}",
            file.display()
        )
    };
    let wire = [
        artifact("a", "\"fresh\":false,", &rlib),
        artifact("b", "\"fresh\":true,", &root.join("b.rlib")),
        artifact("c", "\"fresh\":true,", &root.join("c.rlib")),
        artifact("d", "", &root.join("gone.rlib")),
        "{\"reason\":\"build-finished\",\"success\":true}".to_owned(),
    ]
    .join("\n");
    std::fs::write(root.join("wire"), format!("{wire}\n")).expect("wire");
    let c = Ctx::new(
        root.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        root.clone(),
    );
    let script = "[ \"$1 $2 $3\" = \"--keep-going --message-format=json-render-diagnostics --\" ] \
                  || { echo \"flag misplaced: $*\" >&2; exit 3; }; /bin/cat wire; \
                  echo 'a person reads this'; echo 'warning: rendered for a person' >&2";
    let cmd = Cmd::new("/bin/sh").args([
        "-c",
        script,
        "compile-fixture",
        "--keep-going",
        "--",
        "-D",
        "warnings",
    ]);
    let mut r = Report::new("tippy lint (-p a)");
    assert!(compile_labeled(&c, &mut r, "tippy -p a", &cmd));
    let block = r.render();
    assert!(
        block.contains("  compile tippy lint/tippy -p a: 2 fresh / 1 compiled / 1 unknown, ")
            && block.contains(&format!(", {} out", aterm_capacity::size(3 << 20))),
        "{block}"
    );
    assert!(!block.contains("{\"reason\""), "{block}");
    assert!(
        block.contains("a person reads this") && block.contains("rendered for a person"),
        "{block}"
    );
    assert_eq!(
        r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
        [crate::Outcome::Ok]
    );
    let flagged = |args: &[&str]| {
        crate::compile_account::with_message_format(Cmd::new("targo").args(args.to_vec()))
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        flagged(&["build", "--message-format=json", "--", "-x"]),
        ["build", "--message-format=json", "--", "-x"]
    );
    assert_eq!(
        flagged(&["build", "-q"]),
        ["build", "-q", crate::compile_account::MESSAGE_FORMAT]
    );
    assert_eq!(
        flagged(&["test", "--", "--message-format=x"]),
        [
            "test",
            crate::compile_account::MESSAGE_FORMAT,
            "--",
            "--message-format=x"
        ]
    );
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn a_successful_clean_compile_indexes_its_executables_but_a_failure_or_edit_does_not() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let target = root.join("target");
    std::fs::create_dir_all(target.join("tools")).unwrap();
    // This probe is a test double, not a compiler; echo supplies observable identity
    // output without copying an Apple executable into scratch.
    std::os::unix::fs::symlink("/bin/echo", target.join("tools/trustc")).unwrap();
    let binary = target.join("aterm_gui-0123456789abcdef");
    std::fs::write(&binary, b"binary").unwrap();
    let wire = aterm_json::json!({"reason":"compiler-artifact", "package_id":"aterm-gui", "manifest_path":root.join("crates/aterm-gui/Cargo.toml"), "target":{"kind":["lib"],"name":"aterm_gui"}, "profile":{"test":true}, "executable":binary}).to_string();
    std::fs::write(root.join("wire"), wire).unwrap();
    std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Andrew Yates",
            "-c",
            "user.email=test@aterm.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "compile input",
        ],
    ] {
        assert!(
            aterm_uds::spawnfd::command("git")
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let mut ctx = Ctx::new(
        root.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        target.clone(),
    );
    ctx.tools.targo = target.join("tools/targo");
    let path = ctx.state_dir().join("test-executables.json");
    for (script, code, indexed) in [
        ("/bin/cat wire; exit \"$1\"", "0", true),
        ("/bin/cat wire; exit \"$1\"", "3", false),
        ("/bin/cat wire; /usr/bin/touch changed", "0", false),
    ] {
        let _ = std::fs::remove_file(&path);
        let cmd = Cmd::new("/bin/sh").args(["-c", script, "compile-index", code]);
        let mut report = Report::new("test compile");
        let _ = compile_tests(&ctx, &mut report, "test compile", &cmd);
        let index = crate::test_index::Index::read(&path);
        assert_eq!(index.is_some(), indexed, "{}", report.render());
        if let Some(index) = index {
            assert!(
                index
                    .fixture_binary(&index.tree, &index.toolchain, &root)
                    .is_some()
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn compiler_stderr_cannot_supply_unit_artifacts_or_test_executables() {
    let root = crate::mktemp_dir("atv-machine-stream").expect("scratch");
    let wire = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl")
        .replace("\"test\":false", "\"test\":true")
        .replace("\"executable\":null", "\"executable\":\"/synthetic-test\"");
    std::fs::write(root.join("wire"), &wire).expect("wire");
    let c = Ctx::new(
        root.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        root.clone(),
    );
    let mut failures = Vec::new();
    for (source, code) in [("stderr", 0), ("stderr", 3), ("stdout", 3)] {
        let script = if source == "stderr" {
            "printf 'human stdout\\n'; /bin/cat wire >&2; printf 'human stderr\\n' >&2; exit \"$1\""
        } else {
            "printf 'human stdout\\n'; /bin/cat wire; printf 'human stderr\\n' >&2; exit \"$1\""
        };
        let cmd = Cmd::new("/bin/sh").args(["-c", script, "machine-fixture", &code.to_string()]);
        let mut report = Report::new("machine stream");
        let (ok, exes, artifacts) = compile_tests(&c, &mut report, "machine stream", &cmd);
        assert_eq!(ok, code == 0);
        let exes = exes.expect("complete wire");
        let raw = report.render();
        assert!(raw.contains("human stdout") && raw.contains("human stderr"));
        // The test compile is a compile call: one line, its units read from stdout alone.
        let units = if source == "stdout" {
            ": 0 fresh / 1 compiled, "
        } else {
            ": 0 fresh / 0 compiled, "
        };
        let lines = compile_lines(&raw, "machine stream/machine stream");
        if lines.len() != 1 || !lines[0].contains(units) {
            failures.push(format!(
                "{source}/exit{code}: compile lines {lines:?}; expected one with {units:?}"
            ));
        }
        let expected = usize::from(source == "stdout");
        let causes = raw.matches("\"rebuild_reason\":\"changed-env\"").count();
        if (exes.len(), artifacts.len(), causes) != (expected, expected, expected) {
            failures.push(format!(
                "{source}/exit{code}: executables={}, artifacts={}, causes={causes}; expected {expected}",
                exes.len(), artifacts.len()
            ));
        }
    }
    for code in [0, 3] {
        let cmd = Cmd::new("/bin/sh").args([
            "-c",
            "printf '\\377\\n'; /bin/cat wire; printf 'human stderr\\n' >&2; exit \"$1\"",
            "invalid-fixture",
            &code.to_string(),
        ]);
        let mut report = Report::new("invalid machine stream");
        let (ok, exes, artifacts) = compile_tests(&c, &mut report, "invalid stream", &cmd);
        assert!(!ok);
        assert!(exes.is_err() && artifacts.is_empty());
        let raw = report.render();
        let lines = compile_lines(&raw, "invalid machine stream/invalid stream");
        assert!(
            lines.len() == 1 && lines[0].contains(": units unreported, "),
            "{raw}"
        );
        let actual = if code == 0 {
            crate::Outcome::Ok
        } else {
            crate::Outcome::Fail(crate::Severity::GateFailed)
        };
        assert_eq!(
            report.outcomes().next().map(|(outcome, _)| outcome),
            Some(actual)
        );
        assert!(
            report.outcomes().any(|(outcome, _)| {
                outcome == crate::Outcome::Fail(crate::Severity::CouldNotRun)
            })
        );
        assert!(report.render().contains("human stderr"));
    }
    std::fs::remove_dir_all(&root).expect("cleanup");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The compile lines of `call` (`<step>/<label>`) in a rendered report.
fn compile_lines<'a>(block: &'a str, call: &str) -> Vec<&'a str> {
    let head = format!("  compile {call}: ");
    block.lines().filter(|l| l.starts_with(&head)).collect()
}

/// A DRIVEN BINARY'S BUILD WRITES ITS ONE COMPILE LINE, BUILT OR NOT: the build of a binary a
/// stage drives is a compile call ([`run_compile`]) whose line, its units and the bytes it
/// wrote read from Cargo's messages, stands in its report whether the build passes (the line
/// alone, no row) or fails (its output with what it printed for a person, the line, its FAIL
/// row and the row of what it kept from running), and no Cargo message reaches the report.
#[cfg(unix)]
#[test]
fn a_driven_binarys_build_writes_its_compile_line_whether_it_builds_or_not() {
    let tmp =
        std::fs::canonicalize(crate::mktemp_dir("atv-drive-build").expect("mktemp")).expect("real");
    std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
    let built = tmp.join("libdriven.rlib");
    std::fs::write(&built, vec![0_u8; 2 << 20]).expect("rlib");
    let targo = tmp.join("bin/targo");
    let mut c = Ctx::new(
        tmp.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        tmp.join("scratch"),
    );
    c.tools.targo = targo.clone();
    for code in [0, 1] {
        crate::standin_body(
            &targo,
            &format!(
                "printf '%s\\n' '{{\"reason\":\"compiler-artifact\",\"fresh\":false,\"filenames\":[\"{}\"]}}'\n\
                 echo 'printed for a person'\nexit {code}",
                built.display()
            ),
        );
        let mut r = Report::new("redraw conformance (the verb matrix)");
        let ok = build_to_drive(
            &c,
            &mut r,
            vec!["build".into(), "--bin".into(), "driven".into()],
            "targo build --bin driven".into(),
            "the verb matrix",
        );
        assert_eq!(ok, code == 0);
        let block = r.render();
        let lines = compile_lines(&block, "redraw conformance/targo build --bin driven");
        let out = format!(", {} out", aterm_capacity::size(2 << 20));
        assert!(
            lines.len() == 1
                && lines[0].contains(": 0 fresh / 1 compiled, ")
                && lines[0].ends_with(&out),
            "{block}"
        );
        assert!(!block.contains("{\"reason\""), "{block}");
        let outcomes: Vec<crate::Outcome> = r.outcomes().map(|(o, _)| o).collect();
        if code == 0 {
            assert!(outcomes.is_empty(), "{block}");
        } else {
            assert_eq!(
                outcomes,
                [
                    crate::Outcome::Fail(crate::Severity::GateFailed),
                    crate::Outcome::Fail(crate::Severity::CouldNotRun)
                ],
                "{block}"
            );
            assert!(block.contains("printed for a person"), "{block}");
        }
    }
    std::fs::remove_dir_all(&tmp).ok();
}

fn fixture_gated() -> Vec<(String, String)> {
    let root = gated_fixture_root();
    let gated = gated_lint_features(&root);
    std::fs::remove_dir_all(&root).ok();
    gated
}

/// A checkout whose `crates/*/Cargo.toml` give aterm-gui and aterm-scrollback
/// `required-features` targets and aterm-core none.
fn gated_fixture_root() -> PathBuf {
    let root = crate::mktemp_dir("atv-gated").expect("mktemp");
    for (dir, manifest) in [
        (
            "gui",
            "[package]\nname = \"aterm-gui\"\n\n[[bin]]\nname = \"harness\"\n\
             required-features = [\"control-conformance\"]\n\n[[bench]]\nname = \"b\"\n\
             required-features = [\"bench-support\"]\n",
        ),
        (
            "scrollback",
            "[package]\nname = \"aterm-scrollback\"\n\n[[bench]]\nname = \"x\"\n\
             required-features = [\"disk-tier\"]\n\n[[bench]]\nname = \"y\"\n\
             required-features = [\"disk-tier\"]\n",
        ),
        ("core", "[package]\nname = \"aterm-core\"\n"),
    ] {
        std::fs::create_dir_all(root.join("crates").join(dir)).expect("mkdir");
        std::fs::write(root.join("crates").join(dir).join("Cargo.toml"), manifest).expect("write");
    }
    std::fs::create_dir_all(root.join("crates/no-manifest")).expect("mkdir");
    root
}

/// A cone of `crates`, of which the change touched `touched` and `direct` depend on one directly.
fn gated_cone(crates: &[&str], touched: &[&str], direct: &[&str]) -> Scope {
    let owned = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();
    Scope::changed("main", owned(crates), true)
        .with_seeds(owned(touched))
        .with_all_targets(owned(touched))
        .with_direct_dependents(owned(direct))
}

/// The `-p` packages and the `--features` of the required-features pass among `runs`, or
/// `None` when none is.
fn gated_pass(runs: &[TippyPass]) -> Option<(Vec<String>, String)> {
    let pass = runs
        .iter()
        .find(|run| run.label.starts_with("required-features targets"))?;
    let at = pass.args.iter().position(|a| a == "--features")?;
    let packages = pass.args[..at]
        .windows(2)
        .filter(|w| w[0] == "-p")
        .map(|w| w[1].clone())
        .collect();
    Some((packages, pass.args[at + 1].clone()))
}

/// A change cone lints the `required-features` targets of the packages it touched and of those
/// one edge from them, under the ordinary passes' feature unification, after them and before
/// the shipped build's: 1bda125b88 narrowed a method of aterm-gui's to `cfg(test)` that its
/// `bench-support` module still called, the cone's lint never compiled that module, and the
/// release's lint was the first that did (0.189.0, E0599). The workspace and `-p` lint those of
/// every package they select. CONTROLS: a package with gated targets the cone holds only further
/// away, and a cone holding no package with gated targets, get no such pass.
#[test]
fn a_cone_lints_the_gated_targets_of_what_it_touched_and_what_names_it() {
    let gated = fixture_gated();
    let gui = (
        vec!["aterm-gui".to_string()],
        "aterm-gui/bench-support,aterm-gui/control-conformance".to_string(),
    );
    let touched = gated_cone(&["aterm", "aterm-gui"], &["aterm-gui"], &["aterm"]);
    let runs = lint_invocations(&touched, &gated);
    assert_eq!(gated_pass(&runs), Some(gui.clone()), "{runs:?}");
    let at = runs
        .iter()
        .position(|run| run.label.starts_with("required-features"))
        .expect("the gated pass");
    assert!(
        at == tippy_passes(&touched).len() && runs[at + 1].label == "-p aterm --release",
        "after the ordinary passes, before the shipped build's: {runs:?}"
    );
    assert_eq!(
        runs[at].args,
        lint_unified(tippy_gated_args(&touched, &gated).expect("gated")),
        "the ordinary passes' feature unification, so it shares their dependency units"
    );
    assert!(runs[at].args.ends_with(&LINT_TAIL.map(String::from)));
    let named = gated_cone(
        &["aterm-core", "aterm-gui", "aterm-scrollback"],
        &["aterm-core"],
        &["aterm-gui"],
    );
    assert_eq!(
        gated_pass(&lint_invocations(&named, &gated)),
        Some(gui),
        "a direct dependent's gated targets, not those of one further away"
    );
    let further = gated_cone(&["aterm-core", "aterm-scrollback"], &["aterm-core"], &[]);
    assert_eq!(gated_pass(&lint_invocations(&further, &gated)), None);
    let none = gated_cone(
        &["aterm-core", "aterm-verify"],
        &["aterm-core"],
        &["aterm-verify"],
    );
    let runs = lint_invocations(&none, &gated);
    assert_eq!(gated_pass(&runs), None);
    assert!(
        runs.iter()
            .all(|run| !run.args.iter().any(|a| a == "--features")),
        "{runs:?}"
    );
    assert_eq!(
        gated_pass(&lint_invocations(&Scope::workspace(), &gated)),
        Some((
            vec!["aterm-gui".to_string(), "aterm-scrollback".to_string()],
            "aterm-gui/bench-support,aterm-gui/control-conformance,aterm-scrollback/disk-tier"
                .to_string()
        ))
    );
    assert_eq!(
        gated_pass(&lint_invocations(
            &Scope::crate_only("aterm-scrollback"),
            &gated
        )),
        Some((
            vec!["aterm-scrollback".to_string()],
            "aterm-scrollback/disk-tier".to_string()
        ))
    );
}

/// The lint stage of a dev run, not a release, runs a cone's gated pass: a recording
/// `targo-tippy` stand-in, over the fixture's manifests, sees `--features aterm-gui/…` for a
/// cone that touched aterm-gui. CONTROL: for a cone holding no package with gated targets it
/// sees no `--features`, and as many runs as that cone has ordinary passes.
#[cfg(unix)]
#[test]
fn a_dev_lint_stage_runs_the_cones_gated_pass() {
    let root = gated_fixture_root();
    std::fs::create_dir_all(root.join("scratch")).expect("the run's scratch");
    let argv = |scope: Scope| -> Vec<String> {
        let log = root.join(format!("tippy-{}.log", scope.label().replace(' ', "_")));
        let bin = root.join("bin/targo-tippy");
        crate::standin_body(
            &bin,
            &format!("printf '%s\\n' \"$*\" >> '{}'", log.display()),
        );
        let mut ctx = Ctx::new(
            root.clone(),
            scope,
            EnvSnapshot::default(),
            root.join("scratch"),
        );
        assert!(!ctx.release, "a dev run");
        ctx.tools.tippy = Some(bin);
        let mut r = Report::new("lint".to_string());
        tippy(&ctx, &mut r);
        assert!(
            r.outcomes().all(|(o, _)| o == Outcome::Ok),
            "{}",
            r.render()
        );
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    let touched = argv(gated_cone(&["aterm-gui"], &["aterm-gui"], &[]));
    let none = argv(gated_cone(&["aterm-core"], &["aterm-core"], &[]));
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(touched.len(), 2, "{touched:?}");
    assert!(
        touched[1].contains(
            "-p aterm-gui --features aterm-gui/bench-support,aterm-gui/control-conformance \
             --all-targets --keep-going"
        ) && touched[1].ends_with(" -- -D warnings"),
        "{touched:?}"
    );
    assert_eq!(none.len(), 1, "{none:?}");
    assert!(!none[0].contains("--features"), "{none:?}");
}

#[test]
fn a_measuring_plan_builds_its_benches_once_and_before_the_quiet_machine() {
    use crate::checks::{Check, RELEASE};
    use crate::plan::{folds_measuring, named_skip};
    let benches = measuring_bench_args();
    assert_eq!(benches.len(), 2 * MEASURING_TARGETS.len());
    let app = || Some(crate::MeasureArtifact::unnamed("/release/aterm.app"));
    let narrowed = || Scope::packages(&["aterm-verify".to_string()]);
    let contexts = [
        ctx(Scope::workspace())
            .with_checks(&RELEASE)
            .with_release(true)
            .with_measure_artifact(app()),
        ctx(Scope::workspace())
            .with_checks(&RELEASE)
            .with_release(true),
        ctx(Scope::workspace())
            .with_checks(&[Check::Test])
            .with_checkers_in_tests(true)
            .with_compile_only(true),
        ctx(narrowed())
            .with_checks(&[Check::Test])
            .with_checkers_in_tests(true),
        ctx(Scope::workspace())
            .with_checks(&[Check::Test, Check::Measure])
            .with_measure_artifact(app()),
        ctx(narrowed())
            .with_checks(&[Check::Test, Check::Measure])
            .with_measure_artifact(app()),
        ctx(Scope::workspace())
            .with_checks(&[Check::Measure])
            .with_measure_artifact(app()),
        ctx(Scope::workspace()).with_checks(&[Check::Test]),
    ];
    for c in &contexts {
        let ids: Vec<StageId> = crate::plan::plan(c).iter().map(|s| s.id).collect();
        let [compile, run] = test_cmds(c, false);
        let (compile, run) = (compile.argv(), run.argv());
        let folded = compile.iter().any(|a| a == "--bench");
        assert_eq!(folded, folds_measuring(c), "{:?}: {compile:?}", c.checks);
        if folded {
            assert!(
                compile
                    .windows(benches.len())
                    .any(|w| w == benches.as_slice())
            );
            assert!(compile.iter().any(|a| a == "--tests"), "{compile:?}");
        }
        assert!(
            !run.iter().any(|a| a == "--bench"),
            "the run child runs no bench: {run:?}"
        );
        let prebuilt = ids.contains(&StageId::MeasurePrebuild);
        let compiled = folded && ids.contains(&StageId::TestCompile);
        assert!(!(prebuilt && compiled), "built twice: {:?}", c.checks);
        if ids.contains(&StageId::MeasuringTests) && !named_skip(c, StageId::MeasuringTests) {
            assert!(
                prebuilt || compiled,
                "the quiet stage would compile its benches: {:?}",
                c.checks
            );
        }
    }
}

#[test]
fn the_measuring_stage_does_not_run_behind_the_failed_compile_that_builds_its_benches() {
    use crate::checks::{Check, RELEASE};
    let behind = "the test compile that builds its benches failed";
    let outcome = |c: Ctx| {
        let mut c =
            c.with_measure_artifact(Some(crate::MeasureArtifact::unnamed("/release/aterm.app")));
        c.tools.targo = PathBuf::from("/usr/bin/false");
        c.tools.refused = None;
        c.test_compile_failed
            .store(true, std::sync::atomic::Ordering::Release);
        let mut r = Report::new("measuring");
        measuring_tests(&c, &mut r);
        r.outcomes()
            .next()
            .map(|(o, l)| (o, l.to_string()))
            .expect("a decision")
    };
    let release = ctx(Scope::workspace())
        .with_checks(&RELEASE)
        .with_release(true);
    let (o, label) = outcome(release);
    assert_eq!(
        o,
        crate::Outcome::Fail(crate::Severity::CouldNotRun),
        "{label}"
    );
    assert!(
        label.contains(behind) && label.ends_with(NOT_RUN_BEHIND),
        "{label}"
    );
    // NEGATIVE CONTROL: a run whose prebuild row built the benches measures whatever its
    // test compile did.
    let dev = ctx(Scope::workspace()).with_checks(&[Check::Test, Check::Measure]);
    let (_, label) = outcome(dev);
    assert!(!label.contains(behind), "{label}");
}

#[test]
fn the_measuring_stage_hands_paint_and_spin_the_bundle_it_was_handed() {
    let c = ctx(Scope::workspace());
    let artifact = crate::MeasureArtifact::unnamed("/release/aterm.app");
    let env_of = |cmd: &Cmd, key: &str| {
        cmd.envs
            .iter()
            .find(|(k, _)| k.to_str() == Some(key))
            .map(|(_, v)| PathBuf::from(v))
    };
    let (_, run) = measuring_cmd(&c, &artifact, measuring_args());
    let (_, prebuild) = measuring_targo(&c, measure_prebuild_args());
    for var in ["ATERM_PAINT_BIN", "ATERM_SPIN_BIN"] {
        assert_eq!(
            env_of(&run, var),
            Some(PathBuf::from("/release/aterm.app/Contents/MacOS/aterm")),
            "{var}"
        );
        assert_eq!(
            env_of(&prebuild, var),
            None,
            "the compile needs no bundle: {var}"
        );
    }
    assert_eq!(
        env_of(&run, "ATERM_MEASURE_NO_WINDOWS"),
        None,
        "windows by default"
    );
    let quiet = ctx(Scope::workspace()).with_no_windows(true);
    let (_, quiet_run) = measuring_cmd(&quiet, &artifact, measuring_args());
    assert_eq!(
        env_of(&quiet_run, "ATERM_MEASURE_NO_WINDOWS"),
        Some(PathBuf::from("1")),
        "--no-windows leaves the window rows out"
    );
    assert!(
        !run.argv().iter().any(|a| a == "build"),
        "the stage runs the suites and builds nothing: {:?}",
        run.argv()
    );
    let (pre, rows) = (prebuild.argv(), run.argv());
    assert_eq!(pre[..pre.len() - 1], rows, "{pre:?} vs {rows:?}");
    assert!(rows.windows(2).any(|w| w == MAIN_PROFILE));
    assert!(!rows.iter().any(|a| a == "--ignored" || a == "measuring::"));
    assert_eq!(pre.last().map(String::as_str), Some("--no-run"));
    let mut c = ctx(Scope::workspace());
    c.tools.targo = PathBuf::from("/usr/bin/false");
    c.tools.refused = None;
    let mut r = Report::new("measuring");
    measuring_tests(&c, &mut r);
    let (outcome, label) = r.outcomes().next().expect("a decision");
    assert_eq!(outcome, crate::Outcome::Skip, "{label}");
}

#[test]
fn a_glue_row_without_its_browser_could_not_run() {
    use crate::wasm_page::Refused;
    let c = ctx(Scope::workspace());
    let row = |run: crate::perf::LaneRun| {
        let mut r = Report::new("wasm-glue");
        glue_row(&c, &mut r, "wasm-glue (the toy)", "the toy", move |_| run);
        r.outcomes()
            .next()
            .map(|(o, l)| (o, l.to_string()))
            .expect("a row")
    };
    let (outcome, label) = row(Refused::Unavailable(
        "no pinned chrome-headless-shell placed".into(),
    )
    .verdict("the toy's driver"));
    assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun), "{label}");
    assert_eq!(label.matches("fix:").count(), 1, "{label}");
    assert!(
        label.contains("no pinned chrome-headless-shell placed")
            && label.contains("verify provision"),
        "{label}"
    );
    let (outcome, label) = row(crate::perf::LaneRun::skip(
        "no stock pin",
        "targo --unverified verify provision",
    ));
    assert_eq!(outcome, Outcome::Skip, "{label}");
    let (outcome, label) =
        row(Refused::Failed("drive.mjs threw".into()).verdict("the toy's driver"));
    assert_eq!(outcome, Outcome::Fail(Severity::GateFailed), "{label}");
}

#[test]
fn libc_oracle_reads_the_gates_verdict_before_the_machine() {
    let run = |code: Option<i32>, output: &str| exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: code == Some(0),
        output: output.to_string(),
        code,
        spawn_error: None,
        usage: None,
    };
    let full = exec::ENOSPC_SENTENCE;
    for (code, output, want) in [
        (Some(0), "", Outcome::Ok),
        (Some(1), "", Outcome::Fail(Severity::GateFailed)),
        (Some(1), full, Outcome::Fail(Severity::GateFailed)),
        (Some(3), "", Outcome::Fail(Severity::CouldNotRun)),
        (None, "", Outcome::Fail(Severity::CouldNotRun)),
        (Some(101), "", Outcome::Fail(Severity::GateFailed)),
        (Some(101), full, Outcome::Fail(Severity::CouldNotRun)),
        (Some(2), "", Outcome::Fail(Severity::GateFailed)),
    ] {
        assert_eq!(
            libc_oracle_outcome(&run(code, output)),
            want,
            "{code:?} {output:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_machine_with_no_doc_driver_anywhere_is_diagnosed_on_the_doctests_row() {
    let spec = |id| StageSpec {
        id,
        title: "t".into(),
        lane: crate::plan::Lane::MainTarget,
        exclusive: false,
        after_stages: Vec::new(),
        mem: 0,
        part: None,
    };
    let tmp = crate::mktemp_dir("atv-doc-scope").expect("mktemp");
    let documented = tmp.join("crates/documented");
    std::fs::create_dir_all(documented.join("src")).unwrap();
    std::fs::write(
        documented.join("Cargo.toml"),
        "[package]\nname = \"documented\"\n",
    )
    .unwrap();
    std::fs::write(
        documented.join("src/lib.rs"),
        "/// ```\n/// assert!(true);\n/// ```\npub fn f() {}\n",
    )
    .unwrap();
    crate::standin_body(
        &tmp.join("trustc"),
        "printf 'rustc 1.93.0 (trust)\\nbinary: rustc\\nhost: aarch64-apple-darwin\\nrelease: \
         1\\n'",
    );
    let ok_targo = {
        let p = tmp.join("targo");
        crate::standin_body(
            &p,
            "case \"$*\" in\n  *config*get*build.target*)\n    echo 'error: config value \
             `build.target` is not set' >&2\n    exit 101 ;;\nesac\nexit 0",
        );
        p
    };
    let no_driver = |scope| {
        let mut c = ctx(scope);
        c.tools.stage2_dir = tmp.clone();
        c.tools.targo = ok_targo.clone();
        c.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
        c.path_env = "/nonexistent-dir".into();
        c.root = tmp.clone();
        c.scratch = std::env::temp_dir();
        c
    };
    let cc = no_driver(Scope::workspace());

    let r = run_stage(&cc, &spec(StageId::Test));
    let outcomes: Vec<Outcome> = r.outcomes().map(|(o, _)| o).collect();
    assert_eq!(outcomes, [Outcome::Ok], "{}", r.render());

    let r = run_stage(&cc, &spec(StageId::Doctests));
    let outcomes: Vec<_> = r.outcomes().collect();
    assert_eq!(outcomes.len(), 1, "doctests decided once");
    assert_eq!(outcomes[0].0, Outcome::Fail(Severity::CouldNotRun));

    let cb = no_driver(Scope::changed("main", vec!["aterm-lab".into()], false));
    let r = run_stage(&cb, &spec(StageId::Doctests));
    let outcomes: Vec<_> = r.outcomes().collect();
    assert_eq!(outcomes.len(), 1, "bin-only doctests decided once");
    assert_eq!(outcomes[0].0, Outcome::Skip, "{}", outcomes[0].1);

    let bin_only = tmp.join("crates/binonly");
    std::fs::create_dir_all(bin_only.join("src")).unwrap();
    std::fs::write(
        bin_only.join("Cargo.toml"),
        "[package]\nname = \"binonly\"\n",
    )
    .unwrap();
    let mut cx = no_driver(Scope::crate_only("binonly"));
    cx.root = tmp.clone();
    let r = run_stage(&cx, &spec(StageId::Doctests));
    let outcomes: Vec<_> = r.outcomes().collect();
    assert_eq!(outcomes[0].0, Outcome::Skip, "{}", outcomes[0].1);

    std::fs::write(
        bin_only.join("src/lib.rs"),
        "//! ```text\n//! not rust\n//! ```\n",
    )
    .unwrap();
    let r = run_stage(&cx, &spec(StageId::Doctests));
    let outcomes: Vec<_> = r.outcomes().collect();
    assert_eq!(outcomes[0].0, Outcome::Ok, "{}", outcomes[0].1);
    std::fs::write(bin_only.join("src/lib.rs"), "//! ```\n//! 1;\n//! ```\n").unwrap();
    let r = run_stage(&cx, &spec(StageId::Doctests));
    let outcomes: Vec<_> = r.outcomes().collect();
    assert_eq!(
        outcomes[0].0,
        Outcome::Fail(Severity::CouldNotRun),
        "{}",
        outcomes[0].1
    );
    let r = run_stage(&cx, &spec(StageId::Test));
    assert!(r.outcomes().all(|(o, _)| o == Outcome::Ok));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn the_doc_driver_rule_pins_stage2_first_and_diagnoses_absence() {
    assert_eq!(doc_driver_from(true, true, true), DocDriver::Stage2);
    assert_eq!(doc_driver_from(true, false, false), DocDriver::Stage2);
    assert_eq!(doc_driver_from(false, true, true), DocDriver::Ambient);
    assert_eq!(doc_driver_from(false, true, false), DocDriver::Ambient);
    assert_eq!(doc_driver_from(false, false, true), DocDriver::BarePath);
    assert_eq!(doc_driver_from(false, false, false), DocDriver::Absent);
}

#[test]
fn the_doctests_select_exactly_what_the_test_compile_selects() {
    fn selection(argv: &[String]) -> Vec<String> {
        let mut v = Vec::new();
        let mut it = argv.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-p" => v.push(format!("-p {}", it.next().expect("a -p value"))),
                "--workspace" => v.push(a.clone()),
                _ => {}
            }
        }
        v
    }
    for scope in [
        Scope::workspace(),
        Scope::crate_only("aterm-grid"),
        Scope::packages(&["aterm-core".into(), "aterm-types".into()]),
        Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true),
    ] {
        let doc = doctest_args(&scope);
        assert!(!selection(&doc).is_empty(), "{doc:?}");
        assert_eq!(selection(&doc), selection(&test_compile_args(&scope)));
        let mut c = ctx(scope);
        c.tools.trustdoc = PathBuf::from("/stage2/trustdoc");
        for skip in [&[][..], &["plain".to_string()][..]] {
            let cmd = doctest_cmd(&c, &DocDriver::Stage2, skip).expect("an exact driver");
            let unified = aterm_toolchain::build_cache::with_test_graph(&doc);
            assert!(
                cmd.args.ends_with(
                    &unified
                        .iter()
                        .map(std::ffi::OsString::from)
                        .collect::<Vec<_>>()
                ),
                "{:?}",
                cmd.args
            );
        }
    }
}

#[test]
fn the_doctests_split_the_scopes_libraries_by_what_their_docs_hold() {
    let tmp = crate::mktemp_dir("atv-doc-split").expect("mktemp");
    for (name, file, text) in [
        (
            "documented",
            "lib.rs",
            "/// ```\n/// assert!(true);\n/// ```\npub fn f() {}\n",
        ),
        ("plain", "lib.rs", "//! ```text\n//! not rust\n//! ```\n"),
        (
            "binonly",
            "main.rs",
            "/// ```\n/// 1;\n/// ```\nfn main() {}\n",
        ),
    ] {
        let dir = tmp.join("crates").join(name);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        )
        .expect("manifest");
        std::fs::write(dir.join("src").join(file), text).expect("source");
    }
    let split = |scope, root: &std::path::Path| {
        let mut c = ctx(scope);
        c.root = root.to_path_buf();
        doctest_split(&c)
    };
    let names = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        split(Scope::workspace(), &tmp),
        Some(DocSplit {
            run: names(&["documented"]),
            skip: names(&["plain"]),
        })
    );
    assert_eq!(
        split(Scope::packages(&names(&["plain", "ghost"])), &tmp),
        Some(DocSplit {
            run: names(&["ghost"]),
            skip: names(&["plain"]),
        })
    );
    assert_eq!(
        split(Scope::crate_only("plain"), &tmp),
        Some(DocSplit {
            run: Vec::new(),
            skip: names(&["plain"]),
        })
    );
    assert_eq!(split(Scope::workspace(), &tmp.join("no-members")), None);
    std::fs::remove_dir_all(&tmp).ok();
}

fn env_of(cmd: &Cmd, k: &str) -> Option<std::ffi::OsString> {
    cmd.envs
        .iter()
        .rev()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.clone())
}

#[cfg(unix)]
#[test]
fn native_test_compilers_keep_the_captured_layout_through_declared_environment() {
    const PROBE: &str = "ATERM_TEST_NATIVE_LAYOUT_PROBE";
    const TEST: &str = "stages::tests::native_test_compilers_keep_the_captured_layout_through_declared_environment";
    if std::env::var_os(PROBE).is_some() {
        for (actual, expected) in [
            ("CARGO_BUILD_BUILD_DIR", "ATERM_TEST_EXPECT_BUILD_DIR"),
            ("CARGO_HOME", "ATERM_TEST_EXPECT_CARGO_HOME"),
            ("CARGO_TARGET_DIR", "ATERM_TEST_EXPECT_TARGET"),
            ("ATERM_TEST_TOOL_HOME", "ATERM_TEST_EXPECT_TOOL_HOME"),
            (
                "ATERM_TEST_PACKAGE_PREFIX",
                "ATERM_TEST_EXPECT_PACKAGE_PREFIX",
            ),
        ] {
            assert_eq!(
                std::env::var_os(actual),
                std::env::var_os(expected),
                "{actual}"
            );
        }
        assert!(std::env::var_os("CARGO_BUILD_TARGET_DIR").is_none());
        assert_ne!(
            std::env::var_os("HOME"),
            std::env::var_os("ATERM_TEST_EXPECT_TOOL_HOME")
        );
        let native = PathBuf::from(std::env::var_os("ATERM_TEST_EXPECT_PACKAGE_PREFIX").unwrap());
        assert!(
            std::env::split_paths(&std::env::var_os("PATH").unwrap())
                .any(|path| path == native.join("bin"))
        );
        return;
    }
    for from_environment in [false, true] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let root = crate::physical(&temp.path().join("source"));
        let cargo_home = root.join("cargo-settings");
        let tool_home = temp.path().join("tools");
        let native = temp.path().join("native");
        std::fs::create_dir_all(&cargo_home).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        let config = cargo_home.join("config.toml");
        std::fs::write(&config, "[build]\nbuild-dir='config-build.noindex'\n").unwrap();
        let mut layout = vec![("CARGO_HOME", "cargo-settings".into())];
        if from_environment {
            layout.push(("CARGO_BUILD_BUILD_DIR", "env-build.noindex".into()));
        }
        let expected_build = root.join(if from_environment {
            "env-build.noindex"
        } else {
            "config-build.noindex"
        });
        let mut c = Ctx::new(
            root.clone(),
            Scope::workspace(),
            crate::EnvSnapshot {
                home: tool_home.clone(),
                tool_home: Some(tool_home.clone()),
                test_package_prefix: Some(native.clone()),
                native_packages: Some(native.clone()),
                cargo_layout: layout,
                path: std::env::var_os("PATH").unwrap_or_default(),
                ..crate::EnvSnapshot::default()
            },
            temp.path().join("scratch"),
        );
        assert!(c.main_build_dir.is_none());
        assert_eq!(
            c.native_build_plan().unwrap().explicit_build_dir(),
            Some(expected_build.as_path())
        );
        c.native_build_plan().unwrap().prepare().unwrap();
        crate::test_env::prepare(&c).unwrap();
        if from_environment {
            c.env
                .cargo_layout
                .iter_mut()
                .find(|(name, _)| *name == "CARGO_BUILD_BUILD_DIR")
                .unwrap()
                .1 = "later.noindex".into();
        }
        std::fs::write(&config, "[build]\nbuild-dir='later.noindex'\n").unwrap();
        let mut compiler = test_compile_cmd(&c, false, test_compile_args(&c.scope));
        assert!(compiler.exact_env);
        // The real compiler factory and declared boundary select the environment. Replace
        // only its executable/arguments with this binary's bounded environment probe.
        compiler.program = std::env::current_exe().unwrap();
        compiler.args = ["--exact", TEST].map(std::ffi::OsString::from).to_vec();
        compiler = compiler
            .env(PROBE, "1")
            .env("ATERM_TEST_EXPECT_BUILD_DIR", &expected_build)
            .env("ATERM_TEST_EXPECT_CARGO_HOME", &cargo_home)
            .env("ATERM_TEST_EXPECT_TARGET", &c.target_dir)
            .env("ATERM_TEST_EXPECT_TOOL_HOME", &tool_home)
            .env("ATERM_TEST_EXPECT_PACKAGE_PREFIX", &native);
        let ran = exec::run(&compiler, c.exec_env());
        assert!(ran.ok, "{from_environment}: {}", ran.output);
        let mut missing = compiler;
        missing
            .envs
            .retain(|(name, _)| name != "CARGO_BUILD_BUILD_DIR");
        let ran = exec::run(&missing, c.exec_env());
        assert!(
            !ran.ok,
            "dropping the captured choice must break the actual child control"
        );
        assert!(!root.join("later.noindex").exists());
    }
}

#[cfg(unix)]
#[test]
fn checker_policy_is_explicit_and_every_other_child_scrubs_it() {
    const PROBE: &str = "ATERM_SPEC_POLICY_PROBE";
    let temp = aterm_tempfile::tempdir().unwrap();
    let mut c = ctx(Scope::workspace());
    c.root = temp.path().to_owned();
    c.scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&c.scratch).unwrap();
    let trustdoc = temp.path().join("stage2/trustdoc");
    crate::standin_body(&trustdoc, "exit 0");
    c.tools.trustdoc = trustdoc.clone();
    if std::env::var_os(PROBE).is_some() {
        for key in [
            aterm_capacity::RELEASE_RUN_ENV,
            "ATERM_RELEASE_INPUTS_SEAL",
            "ATERM_RELEASE_CUTTER",
        ] {
            let out = exec::run(&Cmd::new("/usr/bin/printenv").arg(key), c.exec_env());
            assert_eq!(out.code, Some(1), "inherited {key}: {}", out.output);
        }
        let out = exec::run(
            &Cmd::new("/usr/bin/printenv").arg(CHECKERS_ENV),
            c.exec_env(),
        );
        assert_eq!(
            out.code,
            Some(1),
            "an inherited checker policy leaked: {}",
            out.output
        );
        for required in [false, true] {
            let out = exec::run(
                &with_checkers(Cmd::new("/usr/bin/printenv").arg(CHECKERS_ENV), required),
                c.exec_env(),
            );
            assert!(out.ok, "{}", out.output);
            assert_eq!(out.output.trim(), if required { "1" } else { "0" });
        }
        return;
    }
    let child = Cmd::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "stages::tests::checker_policy_is_explicit_and_every_other_child_scrubs_it",
        ])
        .env(PROBE, "1")
        .env(aterm_capacity::RELEASE_RUN_ENV, "1")
        .env("ATERM_RELEASE_INPUTS_SEAL", "/owner/seal")
        .env("ATERM_RELEASE_CUTTER", "/owner/cutter")
        .env(CHECKERS_ENV, "1");
    let out = exec::run(&child, c.exec_env());
    assert!(out.ok && out.output.contains("1 passed"), "{}", out.output);

    let mut release = ctx(Scope::workspace()).with_release(true);
    release.tools.trustdoc = trustdoc.clone();
    let mut as_release = ctx(Scope::workspace()).with_checkers_in_tests(true);
    as_release.tools.trustdoc = trustdoc;
    for context in [&c, &release, &as_release] {
        let [compile, run] = test_cmds(context, false);
        let shaped_compile = test_compile_cmd(
            context,
            false,
            vec![
                "--unverified".into(),
                "test".into(),
                "-p".into(),
                "x".into(),
            ],
        );
        assert_eq!(env_of(&compile, CHECKERS_ENV), None);
        let want = Some(if context.checkers_in_tests { "1" } else { "0" }.into());
        assert_eq!(env_of(&run, CHECKERS_ENV), want);
        let marker = None;
        assert_eq!(env_of(&compile, aterm_capacity::RELEASE_RUN_ENV), marker);
        assert_eq!(
            env_of(&shaped_compile, aterm_capacity::RELEASE_RUN_ENV),
            marker
        );
        assert_eq!(env_of(&run, aterm_capacity::RELEASE_RUN_ENV), marker);
        assert!(compile.exact_env);
        assert!(shaped_compile.exact_env);
        assert!(run.exact_env);
        assert_eq!(
            env_of(
                &doctest_cmd(context, &DocDriver::Stage2, &[]).expect("the stage2 doc driver"),
                aterm_capacity::RELEASE_RUN_ENV
            ),
            marker
        );
        assert_eq!(
            env_of(
                &doctest_cmd(context, &DocDriver::Stage2, &[]).expect("the stage2 doc driver"),
                CHECKERS_ENV
            ),
            want
        );
        assert_eq!(run.args, test_cmds(&c, false)[1].args);
    }
    let checker = spec_checkers_cmd(&c);
    assert_eq!(env_of(&checker, CHECKERS_ENV), Some("1".into()));
    assert_eq!(checker.args, test_cmds(&c, false)[1].args);
}

/// RELEASE TEST PLANNING AND COMPILATION SEE THE PREPARED DISPOSABLE HOME: the unit-graph
/// resolver and the actual compile each start only after the fixed release history exists in
/// the exact HOME they receive. NEGATIVE CONTROL: removing that HOME between the two calls
/// requires the compile path to prepare it again.
#[cfg(unix)]
#[test]
fn release_test_planning_and_compilation_prepare_their_declared_home() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let scratch = temp.path().join("scratch");
    let home = scratch.join("release-test-home");
    let record = home.join(".local/state/aterm-release/disk-footprint.json");
    let targo = temp.path().join("bin/targo");
    crate::standin_body(
        &targo,
        &format!(
            "case \" $* \" in *' build '*) exit 0;; esac\n\
             test \"$HOME\" = '{}' || exit 17\n\
             test -z \"$ATERM_RELEASE_RUN\" || exit 18\n\
             case \"$*\" in *--unit-graph*) echo '{{\"version\":1,\"units\":[],\"roots\":[]}}';; esac",
            home.display()
        ),
    );
    let mut c = Ctx::new(
        temp.path().to_owned(),
        Scope::workspace(),
        EnvSnapshot::default(),
        scratch,
    );
    c.tools.targo = targo;
    assert!(!record.exists());

    let piece = crate::shape::Piece::of(&["aterm-verify".to_string()], false);
    assert!(
        unit_graph(
            &c,
            &piece,
            false,
            &mut crate::unitgraph::Interner::default()
        )
        .is_ok()
    );
    assert!(record.exists());

    std::fs::remove_dir_all(&home).unwrap();
    let spec = crate::plan::declared(&c.scope).remove(0);
    let mut report = Report::new(spec.title.clone());
    test_compile(&c, &spec, &mut report);
    assert!(
        !c.test_compile_failed
            .load(std::sync::atomic::Ordering::Acquire),
        "{}",
        report.render()
    );
    assert!(record.exists());
}

/// A RELEASE WORKSPACE'S CLOSING GRAPH INCLUDES EVERY TARGET ITS COMPILE FOLDS: its ordinary
/// test graph includes a unit selected only by a measuring bench. NEGATIVE CONTROL: a shaped
/// cut resolves only its own test command and sees neither that bench nor the sentinel AppKit
/// units; the latter fail this test if their obsolete graph calls return to this stage.
#[cfg(unix)]
#[test]
fn a_release_workspaces_closing_graph_includes_folded_benches() {
    use crate::unitgraph::tests::graph;

    let temp = aterm_tempfile::tempdir().expect("scratch");
    let targo = temp.path().join("bin/targo");
    let base = graph(&[("base", "base", "test", "test", "0", &[], &[])]);
    let folded = graph(&[
        ("base", "base", "test", "test", "0", &[], &[]),
        ("bench-only", "bench_only", "bench", "test", "0", &[], &[]),
    ]);
    let redraw = graph(&[("redraw-only", "redraw_only", "bin", "build", "0", &[], &[])]);
    let objc = graph(&[("objc-only", "objc_only", "example", "build", "0", &[], &[])]);
    crate::standin_body(
        &targo,
        &format!(
            "case \"$*\" in\n\
             *--bench*--unit-graph*) printf '%s\\n' '{folded}' ;;\n\
             *--bin*aterm-redraw-conformance*--unit-graph*) printf '%s\\n' '{redraw}' ;;\n\
             *--example*--unit-graph*) printf '%s\\n' '{objc}' ;;\n\
             *--unit-graph*) printf '%s\\n' '{base}' ;;\n\
             *) exit 0 ;;\n\
             esac",
        ),
    );
    let mut c = Ctx::new(
        temp.path().to_owned(),
        Scope::workspace(),
        EnvSnapshot::default(),
        temp.path().join("scratch"),
    )
    .with_checks(&[crate::checks::Check::Test])
    .with_checkers_in_tests(true);
    c.tools.targo = targo;
    let piece = crate::shape::Piece::of(&["base".to_owned()], false);
    let closing = unit_graph(&c, &piece, true, &mut crate::unitgraph::Interner::default())
        .expect("closing graph");
    let packages: std::collections::BTreeSet<&str> = closing
        .names()
        .into_iter()
        .map(|name| name.package.as_str())
        .collect();
    let expected = std::collections::BTreeSet::from(["base", "bench-only"]);
    assert_eq!(packages, expected);

    let cut = unit_graph(
        &c,
        &piece,
        false,
        &mut crate::unitgraph::Interner::default(),
    )
    .expect("cut graph");
    assert_eq!(
        cut.names()
            .into_iter()
            .map(|name| name.package.as_str())
            .collect::<Vec<_>>(),
        ["base"]
    );
}

#[test]
fn the_stand_in_drives_rustdoc_only_where_the_scan_cleared_a_library() {
    let (driver, skip_env) = (crate::docdriver::DRIVER_ENV, crate::docdriver::SKIP_ENV);
    let stage2 = std::path::absolute("/stage2/trustdoc").expect("absolute");
    let caller = std::path::absolute("/caller/rustdoc").expect("absolute");
    let recorder = std::path::absolute("/bin/aterm-verify").expect("absolute");
    let mut c = ctx(Scope::workspace());
    c.tools.trustdoc = stage2.clone();
    c.test_recorder = Some(recorder.clone());
    c.env.rustdoc_override = Some(caller.clone().into_os_string());
    c = c.with_checkers_in_tests(true);
    let skip = ["other".to_string(), "plain".to_string()];
    for (kind, real) in [(DocDriver::Stage2, &stage2), (DocDriver::Ambient, &caller)] {
        let cmd = doctest_cmd(&c, &kind, &skip).expect("an exact driver");
        assert_eq!(
            env_of(&cmd, "RUSTDOC"),
            Some(recorder.clone().into_os_string()),
            "{kind:?}"
        );
        assert_eq!(
            env_of(&cmd, driver),
            Some(real.clone().into_os_string()),
            "{kind:?}"
        );
        assert_eq!(
            env_of(&cmd, skip_env),
            Some("other,plain".into()),
            "{kind:?}"
        );
        let cmd = doctest_cmd(&c, &kind, &[]).expect("an exact driver");
        assert_eq!(
            env_of(&cmd, driver),
            Some(real.clone().into_os_string()),
            "{kind:?}"
        );
        assert_eq!(env_of(&cmd, skip_env), None, "{kind:?}");
        assert_eq!(
            env_of(&cmd, "RUSTDOC"),
            Some(recorder.clone().into_os_string()),
            "{kind:?}"
        );
        assert_eq!(
            env_of(&cmd, crate::docdriver::RUNTOOL_ENV),
            Some(recorder.clone().into_os_string()),
            "{kind:?}"
        );
        for inherited in [
            crate::docdriver::DRIVER_ENV,
            crate::docdriver::SKIP_ENV,
            crate::docdriver::RUNTOOL_ENV,
            crate::docdriver::RUNTOOL_SCRATCH_ENV,
            crate::docdriver::RUNTOOL_SAMPLES_ENV,
            crate::docdriver::RUNTOOL_HALT_ENV,
        ] {
            assert!(
                cmd.remove_envs.iter().any(|name| name == inherited),
                "{kind:?} did not scrub inherited {inherited}"
            );
        }
    }
    let compile = test_compile_cmd(&c, false, test_compile_args(&c.scope));
    assert!(compile.exact_env);
    assert_eq!(env_of(&compile, "RUSTDOC"), Some(caller.into_os_string()));
}

/// THE STAND-IN RUNS THE FILE CARGO WOULD HAVE RUN, named absolutely. With
/// no override cargo takes the plain `trustdoc` beside the targo it runs,
/// the file a link to it resolves to, before PATH; a bare name the stand-in
/// looked up on its own PATH could be another file.
#[cfg(unix)]
#[test]
fn the_stand_in_is_told_the_driver_cargo_resolves() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let tmp =
        std::fs::canonicalize(crate::mktemp_dir("atv-doc-driver").expect("mktemp")).expect("real");
    let file = |p: &std::path::Path| {
        std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
        std::fs::write(p, "").expect("write");
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    };
    let (build, links, path) = (tmp.join("build"), tmp.join("links"), tmp.join("path"));
    for f in [
        build.join("targo"),
        build.join("trustdoc"),
        path.join("trustdoc"),
    ] {
        file(&f);
    }
    file(&tmp.join("elsewhere/trustdoc"));
    std::fs::create_dir_all(&links).expect("mkdir");
    symlink(build.join("targo"), links.join("targo")).expect("symlink");
    let mut c = ctx(Scope::workspace());
    c.tools.targo = links.join("targo");
    c.tools.trustdoc = links.join("trustdoc");
    c.path_env = std::env::join_paths([&links, &path]).expect("join");
    c.test_recorder = Some(PathBuf::from("/bin/aterm-verify"));
    let resolved = |c: &Ctx, kind| cargo_doc_driver(c, &kind);
    assert_eq!(
        resolved(&c, DocDriver::BarePath),
        Some(build.join("trustdoc"))
    );
    let cmd =
        doctest_cmd(&c, &DocDriver::BarePath, &["plain".to_string()]).expect("the resolved driver");
    assert_eq!(env_of(&cmd, "RUSTDOC"), Some("/bin/aterm-verify".into()));
    assert_eq!(
        env_of(&cmd, crate::docdriver::DRIVER_ENV),
        Some(build.join("trustdoc").into_os_string())
    );
    c.env.rustdoc_override = Some("".into());
    assert_eq!(
        resolved(&c, DocDriver::Ambient),
        Some(build.join("trustdoc"))
    );
    for (value, want) in [
        ("/caller/rustdoc", Some(PathBuf::from("/caller/rustdoc"))),
        ("tools/rustdoc", Some(PathBuf::from("/repo/tools/rustdoc"))),
        ("trustdoc", Some(path.join("trustdoc"))),
        ("rustdoc", None),
        (
            "tools\\rustdoc",
            Some(PathBuf::from("/repo/tools\\rustdoc")),
        ),
    ] {
        c.env.rustdoc_override = Some(value.into());
        assert_eq!(resolved(&c, DocDriver::Ambient), want, "{value}");
    }
    let root = tmp.join("repo");
    std::fs::create_dir_all(root.join(".cargo")).expect("mkdir");
    let repo = std::mem::replace(&mut c.root, root.clone());
    for (config, named) in [
        (
            "[build]\nrustdocflags = ['--cfg', 'x']\n# rustdoc = 'no'\n",
            true,
        ),
        ("[build]\nrustdoc = '/config/rustdoc'\n", false),
    ] {
        std::fs::write(root.join(".cargo/config.toml"), config).expect("config");
        c.env.rustdoc_override = None;
        assert_eq!(
            resolved(&c, DocDriver::BarePath).is_some(),
            named,
            "{config}"
        );
        c.env.rustdoc_override = Some("".into());
        assert_eq!(
            resolved(&c, DocDriver::Ambient).is_some(),
            named,
            "{config}"
        );
    }
    c.env.rustdoc_override = Some("/caller/rustdoc".into());
    assert_eq!(
        resolved(&c, DocDriver::Ambient),
        Some(PathBuf::from("/caller/rustdoc"))
    );
    assert!(
        doctest_cmd(&c, &DocDriver::BarePath, &["plain".to_string()]).is_err(),
        "a configured driver verify cannot resolve is refused"
    );
    c.root = repo;
    c.env.rustdoc_override = Some("".into());
    std::fs::remove_file(build.join("trustdoc")).expect("rm");
    symlink(tmp.join("elsewhere/trustdoc"), build.join("trustdoc")).expect("symlink");
    assert_eq!(
        resolved(&c, DocDriver::BarePath),
        Some(path.join("trustdoc"))
    );
    c.path_env = links.into_os_string();
    assert_eq!(resolved(&c, DocDriver::BarePath), None);
    assert!(
        doctest_cmd(&c, &DocDriver::BarePath, &["plain".to_string()]).is_err(),
        "an unresolved driver is refused rather than run unmonitored"
    );
    assert_eq!(resolved(&c, DocDriver::Absent), None);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn a_config_names_a_doc_driver_only_by_its_key() {
    for line in [
        "rustdoc = '/x/rustdoc'",
        "rustdoc=\"trustdoc\"",
        "build.rustdoc = 'r'",
        "\"rustdoc\" = 'r'",
        "build = { jobs = 4, rustdoc = 'r' }",
    ] {
        assert!(names_rustdoc(line), "{line}");
    }
    for line in [
        "rustdocflags = ['--cfg', 'docsrs']",
        "my-rustdoc = 'r'",
        "rustc = 'rustdoc'",
        "[build]",
    ] {
        assert!(!names_rustdoc(line), "{line}");
    }
}

/// A narrow test build on the main lane resolves the workspace's features, so it builds only
/// units the workspace build did, by the real `targo`'s unit graphs over a scratch workspace
/// where one member alone turns a shared path dependency's feature on; and the words leave
/// the workspace graph as it was. NEGATIVE CONTROL: the same narrow call without them holds
/// a unit the workspace build never makes (the dependency without its feature).
#[test]
fn a_narrow_main_lane_test_build_builds_only_what_the_workspace_build_did() {
    let directory = aterm_tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver='2'\nmembers=['wide','narrow','shared']\n",
    )
    .unwrap();
    for (name, extra) in [
        ("shared", "[features]\nextra=[]\n"),
        (
            "wide",
            "[dependencies]\nshared={path='../shared',features=['extra']}\n",
        ),
        ("narrow", "[dependencies]\nshared={path='../shared'}\n"),
    ] {
        let package = root.join(name);
        std::fs::create_dir_all(package.join("src")).unwrap();
        std::fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{extra}"),
        )
        .unwrap();
        std::fs::write(package.join("src/lib.rs"), "").unwrap();
    }
    let mut c = ctx(Scope::crate_only("narrow"));
    c.root = root.to_owned();
    c.scratch = root.join("scratch");
    std::fs::create_dir_all(&c.scratch).unwrap();
    c.tools.targo = PathBuf::from(env!("CARGO"));
    c.main_build_dir = None;
    c.child_ceiling = Some(std::time::Duration::from_secs(60));
    let mut interner = crate::unitgraph::Interner::default();
    let mut graph = |cmd: Cmd| {
        // This comparison varies feature scope; keep the main lane's incremental setting
        // on both arms even when the test is run outside verify's own environment.
        let cmd = cmd
            .args(UNIT_GRAPH)
            .arg("--offline")
            .env("CARGO_HOME", root.join("cargo-home"))
            .env("CARGO_INCREMENTAL", "0");
        let out = exec::run(&cmd, c.exec_env());
        assert!(out.ok, "{:?}: {}", cmd.argv(), out.output);
        let text = out
            .output
            .lines()
            .find(|l| l.starts_with("{\"version\":1"))
            .unwrap_or_else(|| panic!("no unit graph: {}", out.output));
        crate::unitgraph::Graph::read(text, &mut interner).unwrap()
    };
    let call = |selection: &[&str]| {
        ["--unverified", "test"]
            .iter()
            .chain(selection)
            .map(|w| (*w).to_owned())
            .collect::<Vec<_>>()
    };
    let wide = graph(main_targo(&c, call(&["--workspace"])));
    let narrow = graph(main_targo(&c, call(&["-p", "narrow"])));
    assert!(narrow.within(&wide), "{:?}", narrow.names());
    // `main_targo` deliberately disables incremental state. Pin the ordinary comparison to
    // the same profile so this assertion isolates the generated workspace-feature words;
    // otherwise Cargo correctly gives every otherwise-identical unit another identity.
    let ordinary_workspace = targo(&c, call(&["--workspace"])).env("CARGO_INCREMENTAL", "0");
    assert_eq!(
        graph(ordinary_workspace).ids(),
        wide.ids(),
        "the words change nothing of the workspace build"
    );
    let plain = graph(targo(&c, call(&["-p", "narrow"])).env("CARGO_INCREMENTAL", "0"));
    assert!(!plain.within(&wide), "the control: {:?}", plain.names());
}

/// The real checkout's dev build of `aterm`, as the shim's rule places it, resolves exactly
/// the unit graph the typed build does (TARGO-MEMBER-FEATURE-SCOPE-1), and that graph's
/// dependency units are all in the typed `targo test -p aterm`'s: one profile, and one
/// dependency shape in the manifests (B21), with no words chosen for it. CONTROLS: the
/// workspace's feature unification around the build (verify's own lanes' words) and a test
/// profile with debuginfo each build units the test graph lacks. A newly feature-flipped dev
/// dependency fails the unmutated comparison.
#[test]
fn dev_build_reuses_the_typed_test_builds_dependencies() {
    use aterm_toolchain::build_cache::{
        Machine, Tool, read_config, with_build_dir, with_test_graph,
    };

    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = aterm_tempfile::manifest_dir()
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut c = ctx(Scope::crate_only("aterm"));
    c.root = root.clone();
    c.scratch = scratch.path().to_owned();
    c.tools.targo = PathBuf::from(env!("CARGO"));
    c.main_build_dir = None;
    c.child_ceiling = Some(std::time::Duration::from_secs(60));
    let machine = Machine {
        root: None,
        home: root.parent(),
        cwd: Some(&root),
        env: &|_| None,
        read: &read_config,
        read_manifest: &aterm_toolchain::build_cache::read_manifest,
        writable: &|_| Some(true),
    };
    let mut interner = crate::unitgraph::Interner::default();
    let mut graph = |args: Vec<String>| {
        // A cached compile need not retain registry archives. Keep resolution
        // pinned while Cargo obtains any sources needed to inspect the graph.
        let out = exec::run(
            &targo(&c, args).args(UNIT_GRAPH).arg("--locked"),
            c.exec_env(),
        );
        assert!(out.ok, "{}", out.output);
        let text = out
            .output
            .lines()
            .find(|l| l.starts_with("{\"version\":1"))
            .unwrap();
        let mut value: aterm_json::Value = aterm_json::from_str(text).unwrap();
        // The label is not a compiler input; every effective setting stays in the key.
        for unit in value
            .as_object_mut()
            .unwrap()
            .get_mut("units")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            unit.as_object_mut()
                .unwrap()
                .get_mut("profile")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("name");
        }
        crate::unitgraph::Graph::read(&value.to_string(), &mut interner).unwrap()
    };
    let build = ["--unverified", "build", "-p", "aterm"]
        .map(str::to_owned)
        .to_vec();
    let test = ["--unverified", "test", "-p", "aterm"]
        .map(str::to_owned)
        .to_vec();
    let tested = graph(test.clone());
    let dev = graph(with_build_dir(&build, Tool::Targo, &machine).unwrap());
    assert_eq!(
        dev.ids(),
        graph(build.clone()).ids(),
        "placement changed the graph"
    );
    let shared = tested.ids();
    let missing: Vec<_> = dev
        .units
        .iter()
        .filter(|u| !shared.contains(&u.id) && !u.target_kinds.iter().any(|k| k == "bin"))
        .collect();
    assert!(missing.is_empty(), "dev/test dependency drift: {missing:?}");
    assert!(
        !graph(with_test_graph(&build)).within(&tested),
        "feature control"
    );
    let mut changed = test;
    changed.splice(0..0, ["--config".into(), "profile.test.debug=true".into()]);
    assert!(!dev.within(&graph(changed)), "profile control");
}

#[test]
fn every_argv_reaches_its_whole_selection() {
    let has = |argv: &[String], w: &str| argv.iter().any(|a| a == w);
    for scope in [
        Scope::workspace(),
        Scope::crate_only("aterm-grid"),
        Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true),
    ] {
        for argv in [
            test_compile_args(&scope),
            test_run_args(&scope),
            doctest_args(&scope),
        ] {
            assert!(has(&argv, "--no-fail-fast"), "{argv:?}");
            for c in scope.crates() {
                assert!(
                    argv.windows(2).any(|w| w[0] == "-p" && w[1] == *c),
                    "{c}: {argv:?}"
                );
            }
        }
        let lint = tippy_args(&scope);
        let sep = lint.iter().position(|x| x == "--").expect("a -- separator");
        assert!(has(&lint[..sep], "--keep-going") && has(&lint[..sep], "--all-targets"));
        assert!(
            lint[sep..]
                .windows(2)
                .any(|w| w[0] == "-D" && w[1] == "warnings")
        );
        for c in scope.crates() {
            assert!(lint[..sep].contains(c), "{c}: {lint:?}");
        }
    }
    for argv in [measuring_args(), measure_prebuild_args()] {
        assert!(has(&argv, "--no-fail-fast"), "{argv:?}");
    }
    let g = tippy_gated_args(&Scope::workspace(), &fixture_gated()).expect("gated");
    let sep = g.iter().position(|x| x == "--").expect("a -- separator");
    assert!(
        has(&g[..sep], "--keep-going") && has(&g[sep..], "warnings"),
        "{g:?}"
    );
}

#[test]
fn a_run_compared_a_frame_only_when_the_golden_test_passed() {
    let run = |ok: bool, tests: &[&str]| {
        let mut output = format!(
            "     Running unittests src/lib.rs (target/debug/deps/aterm_gpu-1)\n\nrunning {} \
             tests\n",
            tests.len()
        );
        for t in tests {
            output.push_str(&format!("test {t} ... ok\n"));
        }
        exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok,
            output,
            code: Some(i32::from(!ok)),
            spawn_error: None,
            usage: None,
        }
    };
    let compared = "renderer::frame_record_tests::armed_frames_hold_to_their_cpu_twins";
    assert!(!compared_no_frame(&run(true, &[compared])));
    let mut one_thread = run(true, &[]);
    one_thread.output.push_str(&format!(
        "test {compared} ... frame-record: the video take's format is bgra8\nok\n"
    ));
    assert!(
        !compared_no_frame(&one_thread),
        "what one libtest thread prints"
    );
    let mut behind_a_line = run(true, &[]);
    behind_a_line
        .output
        .push_str(&format!("aterm-gpu: test {compared} ... ok\n"));
    assert!(
        !compared_no_frame(&behind_a_line),
        "another test's unfinished stderr line before the name"
    );
    let mut ignored = run(true, &[]);
    ignored
        .output
        .push_str(&format!("test {compared} ... ignored\n"));
    assert!(compared_no_frame(&ignored));
    let mut ignored_behind_a_line = run(true, &[]);
    ignored_behind_a_line
        .output
        .push_str(&format!("aterm-gpu: test {compared} ... ignored\n"));
    assert!(compared_no_frame(&ignored_behind_a_line));
    assert!(compared_no_frame(&run(
        true,
        &["renderer::frame_record_tests::armed_frames_hold_to_their_cpu_twins_on_bundled_faces"]
    )));
    assert!(compared_no_frame(&run(true, &[])));
    assert!(compared_no_frame(&run(
        true,
        &["renderer::frame_record_tests::a_neighbour"]
    )));
    assert!(
        !compared_no_frame(&run(false, &[])),
        "a failed child is the test's verdict"
    );
}

#[test]
fn an_empty_change_selection_compiles_nothing_in_any_of_those_stages() {
    let c = ctx(Scope::changed("main", vec![], true));
    for stage in [
        StageId::TestCompile,
        StageId::Test,
        StageId::Doctests,
        StageId::Tippy,
        StageId::SpecCheckers,
    ] {
        let mut cc = ctx(c.scope.clone());
        cc.tools.targo = PathBuf::from("/bin/sh");
        cc.tools.tippy = Some(PathBuf::from("/bin/sh"));
        let r = run_stage(
            &cc,
            &StageSpec {
                id: stage,
                title: "t".into(),
                lane: crate::plan::Lane::MainTarget,
                exclusive: false,
                after_stages: Vec::new(),
                mem: 0,
                part: None,
            },
        );
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "{stage:?} decided once");
        assert_eq!(outcomes[0].0, crate::ladder::Outcome::Skip, "{stage:?}");
        assert!(
            outcomes[0].1.contains(EMPTY_CONE),
            "{stage:?} said: {}",
            outcomes[0].1
        );
    }
}

#[test]
fn native_builds_keep_units_without_incremental_state() {
    let c = ctx(Scope::crate_only("a"));
    let main = main_targo(&c, test_compile_args(&c.scope));
    let lint = tippy_cmd(&c, std::path::Path::new("tippy"), tippy_args(&c.scope));
    for command in [main, lint] {
        assert!(
            command
                .envs
                .iter()
                .any(|(k, v)| k == "CARGO_INCREMENTAL" && v == "0")
        );
    }
    // Negative control: this policy belongs to verification, not arbitrary
    // commands or the caller's ordinary iterative builds.
    assert!(Cmd::new("targo").envs.is_empty());
}

#[test]
fn a_borrowed_lint_driver_runs_under_its_own_build_and_says_so() {
    use crate::toolchain::TIPPY;
    let env_of = |cmd: &Cmd, key: &str| {
        cmd.envs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.to_string_lossy().into_owned())
    };
    let managed = PathBuf::from("/m/store/trust/9192/bin").join(TIPPY);
    let mut c = ctx(Scope::workspace());
    c.tools.tippy = Some(managed.clone());
    c.tools.borrowed = vec![(TIPPY, managed.clone())];
    let cmd = tippy_cmd(&c, &managed, tippy_args(&c.scope));
    assert_eq!(
        env_of(&cmd, "TRUST_STAGE2_BIN").as_deref(),
        Some("/m/store/trust/9192/bin")
    );
    assert_eq!(
        env_of(&cmd, "PATH")
            .as_deref()
            .and_then(|p| std::env::split_paths(p).next()),
        Some(PathBuf::from("/m/store/trust/9192/bin")),
        "{cmd:?}"
    );
    assert_eq!(
        crate::fmt::borrowed_suffix(&c, TIPPY),
        format!(" (the managed Trust build's {})", managed.display())
    );
    let own = c.tools.stage2_dir.join(TIPPY);
    c.tools.tippy = Some(own.clone());
    c.tools.borrowed.clear();
    let cmd = tippy_cmd(&c, &own, tippy_args(&c.scope));
    assert_eq!(env_of(&cmd, "TRUST_STAGE2_BIN"), None);
    assert_eq!(crate::fmt::borrowed_suffix(&c, TIPPY), "");
}

#[test]
fn exact_lint_commands_remove_outer_authority_but_keep_real_inputs() {
    let mut c = ctx(Scope::crate_only("a"));
    c.child_env_add.push((
        "TRUST_TARGO_NESTED_UNVERIFIED_ENDPOINT".into(),
        "first".into(),
    ));
    let first = exact_package_tippy_commands(&c, std::path::Path::new("tippy"), "a").unwrap();
    assert!(
        first
            .iter()
            .all(|command| command.args.iter().any(|arg| arg == "--unverified"))
    );
    assert!(first.iter().all(|command| {
        command
            .envs
            .iter()
            .all(|(name, _)| !name.to_string_lossy().starts_with("TRUST_TARGO_"))
    }));
    c.child_env_add.last_mut().unwrap().1 = "second".into();
    let second = exact_package_tippy_commands(&c, std::path::Path::new("tippy"), "a").unwrap();
    assert_eq!(first[0].envs, second[0].envs);
    c.child_env_add
        .push(("INPUT_CONTROL".into(), "changed".into()));
    let changed = exact_package_tippy_commands(&c, std::path::Path::new("tippy"), "a").unwrap();
    assert_ne!(first[0].envs, changed[0].envs);
}

#[test]
fn every_row_a_borrowed_lint_driver_decides_names_it() {
    use crate::toolchain::TIPPY;
    let root =
        std::fs::canonicalize(crate::mktemp_dir("atv-lintrows").expect("mktemp")).expect("real");
    std::fs::create_dir_all(root.join("crates/gui")).expect("mkdir");
    std::fs::write(
        root.join("crates/gui/Cargo.toml"),
        "[package]\nname = \"aterm-gui\"\n\n[[bench]]\nname = \"b\"\n\
         required-features = [\"bench-support\"]\n",
    )
    .expect("manifest");
    let managed = root.join("m/bin").join(TIPPY);
    let own = root.join("s2/bin").join(TIPPY);
    let scope = Scope::packages(&["aterm-gpu".to_string(), "aterm-gui".to_string()]);
    let mut c =
        Ctx::new(root.clone(), scope, EnvSnapshot::default(), root.join("t")).with_release(true);
    c.tools.stage2_dir = own.parent().expect("dir").to_path_buf();
    c.tools.tippy = Some(managed.clone());
    c.tools.borrowed = vec![(TIPPY, managed.clone())];
    let rows = |c: &Ctx| {
        let mut r = Report::new("lint");
        tippy(c, &mut r);
        r.outcomes()
            .map(|(o, l)| (o, l.to_string()))
            .collect::<Vec<_>>()
    };
    let said = format!(" (the managed Trust build's {})", managed.display());
    let borrowed = rows(&c);
    assert_eq!(borrowed.len(), 2, "{borrowed:?}");
    assert!(
        borrowed.iter().all(|(_, l)| l.contains(&said)),
        "{borrowed:?}"
    );
    c.tools.tippy = Some(own);
    c.tools.borrowed.clear();
    let theirs = rows(&c);
    assert_eq!(theirs.len(), 2, "{theirs:?}");
    assert!(
        theirs
            .iter()
            .all(|(_, l)| !l.contains("the managed Trust build's")),
        "{theirs:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn compile_only_children_yield_and_children_that_run_code_do_not() {
    let mut c = ctx(Scope::workspace());
    c.tools.tippy = Some(PathBuf::from("/s2/targo-tippy"));

    for bind in [false, true] {
        let [compile, run] = test_cmds(&c, bind);
        assert!(compile.demoted, "targo test --no-run");
        assert!(!run.demoted, "targo test --tests runs code");
        let artifact = crate::MeasureArtifact::unnamed("/release/aterm.app");
        let (_, measuring) = measuring_cmd(&c, &artifact, measuring_args());
        assert!(
            !measuring.demoted,
            "the measuring benches run the paint, spin, aos and rewind guards"
        );
    }
    assert!(
        tippy_cmd(
            &c,
            std::path::Path::new("/s2/targo-tippy"),
            tippy_args(&c.scope)
        )
        .demoted,
        "tippy"
    );
    for args in [redraw_conformance_build_args(), objc_drives_build_args()] {
        assert!(main_build_cmd(&c, args.clone()).demoted, "{args:?}");
    }
    for argv in [
        redraw_conformance_build_args(),
        objc_drives_build_args(),
        spec_check_cmd(&c).argv()[1..].to_vec(),
    ] {
        assert!(argv.windows(2).any(|w| w == MAIN_PROFILE), "{argv:?}");
    }
}

#[test]
fn the_gated_features_are_read_from_the_manifests() {
    assert_eq!(
        manifest_required_features(
            "[package]\nname = \"p\"\n# `required-features` keeps it out = [\"prose\"]\n\
             [[bin]]\nname = \"not-p\"\nrequired-features = [\"a\", \"b\"] # why\n\
             [[bench]]\nname = \"x\"\nrequired-features = [\n    \"c\",\n]\n"
        ),
        [("p", "a"), ("p", "b"), ("p", "c")].map(|(p, f)| (p.to_string(), f.to_string()))
    );
    assert!(manifest_required_features("[workspace]\nrequired-features = [\"a\"]\n").is_empty());
    assert_eq!(
        fixture_gated(),
        [
            ("aterm-gui", "bench-support"),
            ("aterm-gui", "control-conformance"),
            ("aterm-scrollback", "disk-tier"),
        ]
        .map(|(p, f)| (p.to_string(), f.to_string()))
    );
}

#[test]
fn the_gated_pass_narrows_with_the_scope_and_disappears_when_it_has_nothing_to_reach() {
    let gated = fixture_gated();
    let one = tippy_gated_args(&Scope::crate_only("aterm-scrollback"), &gated)
        .expect("aterm-scrollback owns two gated benches");
    let features = one
        .iter()
        .position(|a| a == "--features")
        .expect("features");
    assert_eq!(one[..features], ["-p", "aterm-scrollback"]);
    assert_eq!(one[features + 1], "aterm-scrollback/disk-tier");
    assert_eq!(
        tippy_gated_args(&Scope::crate_only("aterm-core"), &gated),
        None
    );
    assert_eq!(
        tippy_gated_args(
            &Scope::changed("main", vec!["aterm-core".into()], true),
            &gated
        ),
        None
    );
    assert!(
        tippy_gated_args(
            &Scope::changed("main", vec!["aterm-gui".into()], true),
            &gated
        )
        .is_some(),
        "a changed-scope run that rebuilt aterm-gui must still lint its benches"
    );
    let by_spec = tippy_gated_args(
        &Scope::changed("main", vec!["aterm-scrollback@9.9.9".into()], true),
        &gated,
    )
    .expect("the spec names the gated package");
    assert_eq!(
        by_spec[..4],
        [
            "-p",
            "aterm-scrollback@9.9.9",
            "--features",
            "aterm-scrollback/disk-tier"
        ]
    );
}

#[test]
fn the_shipped_build_is_linted_with_debug_assertions_off() {
    let want = [
        "-p",
        "aterm",
        "--release",
        "--keep-going",
        "--",
        "-D",
        "warnings",
    ];
    for scope in [
        Scope::workspace(),
        Scope::changed("main", vec!["aterm-font".into(), "aterm".into()], true),
    ] {
        assert_eq!(tippy_release_args(&scope).expect("holds aterm"), want);
    }
    assert_eq!(tippy_release_args(&Scope::crate_only("aterm-font")), None);
    let row = |release: bool, scope: Scope| {
        let mut c = ctx(scope).with_release(release);
        c.tools.tippy = Some(PathBuf::from("/repo/bin").join(crate::toolchain::TIPPY));
        let mut r = Report::new("lint");
        tippy(&c, &mut r);
        r.outcomes()
            .filter(|(_, l)| l.contains("--release"))
            .count()
    };
    for release in [false, true] {
        assert_eq!(row(release, Scope::crate_only("aterm")), 1);
        assert_eq!(row(release, Scope::workspace()), 1);
        assert_eq!(row(release, Scope::crate_only("aterm-font")), 0);
        assert_eq!(row(release, Scope::changed("main", Vec::new(), false)), 0);
    }
}

/// EACH CALL OF A SHAPED TEST COMPILE RUNS ITS OWN COMMAND, THE CLOSING ONE TODAY'S: a
/// stand-in targo traces each start, and the calls `-p aterm-gui --lib`, `-p aterm-gui` and
/// the run's selection start `test -p aterm-gui ... --lib --no-run`, the same with `--tests`,
/// and the unshaped command byte for byte, each under the main lane's build directory. Once
/// a call fails, the calls behind it are COULD NOT RUN and start nothing. NEGATIVE CONTROL:
/// the same three calls with none failing start three commands.
#[cfg(unix)]
#[test]
fn each_call_of_a_shaped_test_compile_runs_its_own_command() {
    use crate::shape::{Part, Piece};
    let tmp =
        std::fs::canonicalize(crate::mktemp_dir("atv-shape-calls").expect("mktemp")).expect("real");
    let trace = tmp.join("starts");
    let targo = tmp.join("bin/targo");
    crate::standin_body(
        &targo,
        &format!(
            "echo \"$*\" >> '{}'\nread code < '{}'\nexit \"$code\"",
            trace.display(),
            tmp.join("code").display()
        ),
    );
    let gui = vec!["aterm-gui".to_owned()];
    let pair = vec!["aterm-effects".to_owned(), "aterm-gui".to_owned()];
    let lib = Piece::of(&gui, true);
    let cut = Piece::of(&gui, false);
    let whole = Piece::of(&pair, false);
    let calls = |c: &Ctx| -> Vec<StageSpec> {
        let declared = crate::plan::declared(&c.scope).remove(0);
        [
            (&lib, 0, vec![]),
            (&cut, 1, vec![lib.clone()]),
            (&whole, 2, vec![lib.clone(), cut.clone()]),
        ]
        .into_iter()
        .map(|(piece, index, after)| StageSpec {
            part: Some(Part {
                piece: piece.clone(),
                index,
                of: 3,
                after,
                said: String::new(),
                planned: None,
                refused: Vec::new(),
            }),
            ..declared.clone()
        })
        .collect()
    };
    type Rows = Vec<Vec<(crate::Outcome, String)>>;
    let run = |code: &str, scope: Scope| -> (Rows, Vec<String>, Ctx) {
        std::fs::write(tmp.join("code"), code).expect("code");
        std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
        let _ = std::fs::remove_file(&trace);
        let mut c = Ctx::new(
            tmp.clone(),
            scope,
            EnvSnapshot::default(),
            tmp.join("scratch"),
        );
        c.tools.targo = targo.clone();
        let rows = calls(&c)
            .iter()
            .map(|spec| {
                let mut r = Report::new(spec.title.clone());
                test_compile(&c, spec, &mut r);
                r.outcomes().map(|(o, l)| (o, l.to_owned())).collect()
            })
            .collect();
        let starts = std::fs::read_to_string(&trace)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        (rows, starts, c)
    };

    let (rows, starts, c) = run("0", Scope::packages(&pair));
    assert!(
        rows.iter()
            .all(|r| r.iter().all(|(o, _)| *o == crate::Outcome::Ok)),
        "{rows:?}"
    );
    let lead = format!(
        "--config {}",
        c.main_build_dir.as_deref().expect("a build dir")
    );
    let tail = "--no-fail-fast --tests --no-run --message-format=json-render-diagnostics";
    let test = format!(
        "--unverified {} test {}",
        aterm_toolchain::build_cache::TEST_GRAPH_CONFIG.join(" "),
        aterm_toolchain::build_cache::TEST_GRAPH_FLAG
    );
    assert_eq!(
        starts,
        [
            format!(
                "{lead} {test} -p aterm-gui --no-fail-fast --lib --no-run --message-format=json-render-diagnostics"
            ),
            format!("{lead} {test} -p aterm-gui {tail}"),
            test_cmds(&c, false)[0].argv()[1..].join(" "),
        ]
    );
    assert_eq!(
        starts[2],
        format!("{lead} {test} -p aterm-effects -p aterm-gui {tail}")
    );

    let (rows, starts, workspace) = run("0", Scope::workspace());
    assert!(
        rows.iter()
            .all(|r| r.iter().all(|(o, _)| *o == crate::Outcome::Ok)),
        "{rows:?}"
    );
    assert_eq!(
        starts[2],
        test_cmds(&workspace, false)[0].argv()[1..].join(" "),
        "the closing call must retain the exact ordinary workspace command"
    );
    assert!(starts[2].contains(" --workspace "), "{}", starts[2]);

    let (rows, starts, _) = run("101", Scope::packages(&pair));
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert!(matches!(rows[0][0].0, crate::Outcome::Fail(_)), "{rows:?}");
    for behind in &rows[1..] {
        assert_eq!(behind.len(), 1);
        assert_eq!(
            behind[0].0,
            crate::Outcome::Fail(crate::Severity::CouldNotRun)
        );
        assert!(
            behind[0]
                .1
                .contains("an earlier call of this test compile failed"),
            "{behind:?}"
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// A CUT THE CLOSING CALL REBUILT IS REFUSED FOR THE NEXT RUN: a stand-in targo reports the
/// lib-test call building `libaterm_gui-1.rlib`, and the closing call reporting it again,
/// fresh or not. Fresh, the closing call's time line says so and the book keeps no refusal;
/// rebuilt, the book keeps the refusal row the planner reads ([`crate::shape::refusal_key`])
/// and the time line names it. NEGATIVE CONTROL: the fresh run keeps no row.
#[cfg(unix)]
#[test]
fn a_cut_the_closing_call_rebuilt_is_refused_for_the_next_run() {
    use crate::shape::{Part, Piece};
    let tmp =
        std::fs::canonicalize(crate::mktemp_dir("atv-shape-fresh").expect("mktemp")).expect("real");
    std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
    let targo = tmp.join("bin/targo");
    let artifact = |fresh: &str| {
        format!(
            "{{\"reason\":\"compiler-artifact\",\"filenames\":[\"/d/libaterm_gui-1.rlib\"],\
             \"fresh\":{fresh}}}"
        )
    };
    crate::standin_body(
        &targo,
        &format!(
            "read fresh < '{}'\ncase \"$*\" in *--lib*) echo '{}';; *) echo \"$fresh\";; esac",
            tmp.join("closing").display(),
            artifact("false")
        ),
    );
    let gui = vec!["aterm-gui".to_owned()];
    let lib = Piece::of(&gui, true);
    let whole = Piece::of(&gui, false);
    let run = |fresh: &str| -> (crate::memory::Records, Option<String>) {
        std::fs::write(tmp.join("closing"), artifact(fresh)).expect("closing");
        let path = tmp.join(format!("book-{fresh}.json"));
        let mut c = Ctx::new(
            tmp.clone(),
            Scope::packages(&gui),
            EnvSnapshot::default(),
            tmp.join("scratch"),
        )
        .with_memory(crate::memory::Book::at(
            Some(path),
            Some(24 << 30),
            Some(21 << 30),
        ));
        c.tools.targo = targo.clone();
        let declared = crate::plan::declared(&c.scope).remove(0);
        for (piece, index, after) in [(&lib, 0, vec![]), (&whole, 1, vec![lib.clone()])] {
            let spec = StageSpec {
                part: Some(Part {
                    piece: piece.clone(),
                    index,
                    of: 2,
                    after,
                    said: String::new(),
                    planned: None,
                    refused: Vec::new(),
                }),
                ..declared.clone()
            };
            let mut r = Report::new(spec.title.clone());
            test_compile(&c, &spec, &mut r);
            let rendered = r.render();
            let units: Vec<aterm_json::Value> = rendered
                .lines()
                .filter_map(|line| line.strip_prefix("verify-event: "))
                .map(|line| aterm_json::from_str::<aterm_json::Value>(line).unwrap())
                .filter(|event| event["event"].as_str() == Some("compile-unit-finished"))
                .collect();
            assert_eq!(units.len(), 1, "{rendered}");
            assert_eq!(
                units[0]["fresh"].as_bool(),
                Some(piece != &lib && fresh == "true")
            );
            assert_eq!(units[0]["rebuild_reason"], aterm_json::Value::Null);
            assert!(
                r.outcomes().all(|(o, _)| o == crate::Outcome::Ok),
                "{}",
                r.render()
            );
        }
        let said = c
            .shape_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .said
            .remove(&1);
        (c.memory.as_ref().expect("a book").records(), said)
    };
    let refused = crate::shape::refusal_key(&tmp.display().to_string(), &whole, &lib);

    let (records, said) = run("true");
    assert!(records.rows().iter().all(|r| r.key != refused));
    assert_eq!(
        said.as_deref(),
        Some("shape: fresh here: `-p aterm-gui --lib`'s 1 file")
    );

    let (records, said) = run("false");
    assert!(
        records.rows().iter().any(|r| r.key == refused),
        "{:?}",
        records.rows()
    );
    assert!(said.is_some_and(|s| s.contains("`-p aterm-gui --lib` refused: it rebuilt")));

    // A middle cut can discover the rebuild before the exact closing call supplies the
    // complete workspace selection for the refusal key. Custody of that refusal must survive
    // the closing call finding the middle cut's replacement fresh.
    let pending_path = tmp.join("book-pending.json");
    let pending = Ctx::new(
        tmp.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        tmp.join("scratch"),
    )
    .with_memory(crate::memory::Book::at(
        Some(pending_path),
        Some(24 << 30),
        Some(21 << 30),
    ));
    let middle = Piece::of(&["aterm-effects".to_owned()], false);
    let workspace = Piece::of(&["aterm-effects".to_owned(), "aterm-gui".to_owned()], false);
    let part = |piece: &Piece, index: usize, after: Vec<Piece>| Part {
        piece: piece.clone(),
        index,
        of: 3,
        after,
        said: String::new(),
        planned: None,
        refused: Vec::new(),
    };
    let artifact = |fresh| crate::shape::Artifact {
        filenames: vec!["/d/libaterm_gui-1.rlib".to_owned()],
        fresh,
        package: Some("aterm-gui".to_owned()),
        test: true,
    };
    shape_after(
        &pending,
        &part(&lib, 0, Vec::new()),
        &[artifact(false)],
        std::time::SystemTime::now(),
    );
    shape_after(
        &pending,
        &part(&middle, 1, vec![lib.clone()]),
        &[artifact(false)],
        std::time::SystemTime::now(),
    );
    let pending_key = crate::shape::refusal_key(&tmp.display().to_string(), &workspace, &lib);
    assert!(
        pending
            .memory
            .as_ref()
            .expect("book")
            .records()
            .rows()
            .iter()
            .all(|row| row.key != pending_key),
        "only the exact closing selection can persist it"
    );
    shape_after(
        &pending,
        &part(&workspace, 2, vec![lib.clone(), middle]),
        &[artifact(true)],
        std::time::SystemTime::now(),
    );
    assert!(
        pending
            .memory
            .as_ref()
            .expect("book")
            .records()
            .rows()
            .iter()
            .any(|row| row.key == pending_key),
        "the closing workspace selection must persist the middle call's refusal"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// A CUT REBUILT FROM A SOURCE EDITED UNDER THE RUN IS NOT REFUSED: a stand-in targo reports
/// the lib-test call building `deps/libaterm_gui-1.rlib` and the closing call rebuilding it;
/// its dep-info lists `crates/aterm-gui/src/lib.rs`, under the checkout. Modified after the
/// cut began (an edit by the person or another agent in the worktree), the book keeps no
/// refusal and the time line names the source; unchanged since, the cut is refused as the
/// proof's miss. NEGATIVE CONTROL, by hand: with the sources unread, the edited run kept the
/// refusal, and every later run of the selection would have been one call claiming the
/// whole estimate alone.
#[cfg(unix)]
#[test]
fn a_cut_rebuilt_from_a_source_edited_under_the_run_is_not_refused() {
    use crate::shape::{Part, Piece};
    let tmp =
        std::fs::canonicalize(crate::mktemp_dir("atv-shape-edit").expect("mktemp")).expect("real");
    std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
    let deps = tmp.join("build/deps");
    std::fs::create_dir_all(&deps).expect("deps");
    let source = tmp.join("crates/aterm-gui/src/lib.rs");
    std::fs::create_dir_all(source.parent().expect("its folder")).expect("src");
    std::fs::write(&source, "").expect("source");
    let rlib = deps.join("libaterm_gui-1.rlib");
    std::fs::write(
        deps.join("aterm_gui-1.d"),
        format!("{}: crates/aterm-gui/src/lib.rs\n", rlib.display()),
    )
    .expect("dep-info");
    let targo = tmp.join("bin/targo");
    crate::standin_body(
        &targo,
        &format!(
            "echo '{{\"reason\":\"compiler-artifact\",\"filenames\":[\"{}\"],\"fresh\":false}}'",
            rlib.display()
        ),
    );
    let gui = vec!["aterm-gui".to_owned()];
    let lib = Piece::of(&gui, true);
    let whole = Piece::of(&gui, false);
    let run = |at: std::time::SystemTime| -> (crate::memory::Records, Option<String>) {
        std::fs::File::options()
            .write(true)
            .open(&source)
            .and_then(|f| f.set_modified(at))
            .expect("its time");
        let path = tmp.join("book.json");
        let _ = std::fs::remove_file(&path);
        let mut c = Ctx::new(
            tmp.clone(),
            Scope::packages(&gui),
            EnvSnapshot::default(),
            tmp.join("scratch"),
        )
        .with_memory(crate::memory::Book::at(
            Some(path),
            Some(24 << 30),
            Some(21 << 30),
        ));
        c.tools.targo = targo.clone();
        let declared = crate::plan::declared(&c.scope).remove(0);
        for (piece, index, after) in [(&lib, 0, vec![]), (&whole, 1, vec![lib.clone()])] {
            let spec = StageSpec {
                part: Some(Part {
                    piece: piece.clone(),
                    index,
                    of: 2,
                    after,
                    said: String::new(),
                    planned: None,
                    refused: Vec::new(),
                }),
                ..declared.clone()
            };
            let mut r = Report::new(spec.title.clone());
            test_compile(&c, &spec, &mut r);
            assert!(
                r.outcomes().all(|(o, _)| o == crate::Outcome::Ok),
                "{}",
                r.render()
            );
        }
        let said = c
            .shape_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .said
            .remove(&1);
        (c.memory.as_ref().expect("a book").records(), said)
    };
    let refused = crate::shape::refusal_key(&tmp.display().to_string(), &whole, &lib);
    let day = std::time::Duration::from_secs(24 * 3600);
    let now = std::time::SystemTime::now();

    let (records, said) = run(now + day);
    assert!(
        records.rows().iter().all(|r| r.key != refused),
        "{:?}",
        records.rows()
    );
    assert!(
        said.as_deref().is_some_and(|s| s.contains(
            "not refused, its sources changed under the run: `-p aterm-gui --lib` (it rebuilt \
             1 of its artifacts, and `crates/aterm-gui/src/lib.rs` changed after it began)"
        )),
        "{said:?}"
    );

    let (records, said) = run(now - day);
    assert!(records.rows().iter().any(|r| r.key == refused));
    assert!(said.is_some_and(|s| s.contains("`-p aterm-gui --lib` refused: it rebuilt")));
    std::fs::remove_dir_all(&tmp).ok();
}

/// A REFUSAL ITS SELECTION KEEPS READING IS KEPT AGAIN, SO IT OUTLIVES THE RECORD'S CHURN: the
/// one call a refused cut left S1's test compile in keeps the refusal row again as it runs,
/// its newest peak now, so 512 rows kept after the first refusal (the record keeps 512, and
/// lets the one whose newest peak is oldest go) leave it in place. NEGATIVE CONTROL, by hand:
/// with the refusal not kept again, the 512 rows pushed it out, and the next run would have
/// cut S1 the same wrong way.
#[cfg(unix)]
#[test]
fn a_refusal_its_selection_keeps_reading_outlives_the_records_churn() {
    use crate::shape::{Part, Piece};
    let tmp =
        std::fs::canonicalize(crate::mktemp_dir("atv-shape-keep").expect("mktemp")).expect("real");
    std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
    let targo = tmp.join("bin/targo");
    crate::standin_body(&targo, "exit 0");
    let s1 = crate::shape::tests::s1();
    let whole = Piece::of(&s1, false);
    let peel: Vec<String> = s1.iter().filter(|c| *c != "aterm-gui").cloned().collect();
    let peel = Piece::of(&peel, false);
    let path = tmp.join("book.json");
    let refused = crate::shape::refusal_key(&tmp.display().to_string(), &whole, &peel);
    let mut first = crate::memory::Records::default();
    first.push(refused.clone(), 0, 9);
    first.save(&path).expect("saved");
    let mut c = Ctx::new(
        tmp.clone(),
        Scope::packages(&s1),
        EnvSnapshot::default(),
        tmp.join("scratch"),
    )
    .with_memory(crate::memory::Book::at(
        Some(path.clone()),
        Some(24 << 30),
        Some(21 << 30),
    ));
    c.tools.targo = targo;
    let mut part = Part::whole(whole, String::new());
    part.refused = vec![peel];
    let spec = StageSpec {
        part: Some(part),
        ..crate::plan::declared(&c.scope).remove(0)
    };
    let mut r = Report::new(spec.title.clone());
    test_compile(&c, &spec, &mut r);
    assert!(
        r.outcomes().all(|(o, _)| o == crate::Outcome::Ok),
        "{}",
        r.render()
    );
    let mut records = crate::memory::Records::load(&path);
    let rows = u64::try_from(crate::memory::ROWS_KEPT).expect("a count");
    for at in 10..10 + rows {
        records.push(
            crate::memory::Key {
                stage: "Test".to_owned(),
                checkout: format!("/churn/{at}"),
                crates: None,
                variant: None,
            },
            1,
            at,
        );
    }
    assert!(
        records.rows().iter().any(|r| r.key == refused),
        "the refusal went: {} rows",
        records.rows().len()
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// THE TEST COMPILE'S CALL NOTES WHAT IT USED AND, PASSING WHOLE, CLOSES THE NOTE, which the run
/// then keeps ([`crate::disk::note_call`], [`crate::disk::close`], [`crate::disk::keep_used`],
/// `atpkg::build_cache::stale::USED`): from its child's messages, the unit of a test it built in
/// the build directory, the unit of a bin a test needs, which cargo named by its copy in the
/// target alone, and the unit of a build script it ran, with the package it built tests of.
/// NEGATIVE CONTROL: a compile whose child reported nothing keeps no record.
#[cfg(unix)]
#[test]
fn the_test_compile_notes_what_it_used_and_closes_the_note() {
    let tmp = std::fs::canonicalize(crate::mktemp_dir("atv-used").expect("mktemp")).expect("real");
    std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
    let leaf = tmp.join("cache/.cargo-build");
    let target = tmp.join("target");
    let [test, bin, script] = ["1111111111111111", "2222222222222222", "3333333333333333"];
    let out = |hash: &str| leaf.join("debug/build/atpkg").join(hash).join("out");
    let ran = out(bin).join("atpkg_abrew");
    std::fs::create_dir_all(out(bin)).expect("the bin's unit");
    std::fs::write(&ran, b"the bin the tests ran").expect("the bin");
    std::fs::create_dir_all(target.join("debug")).expect("the target");
    let copy = target.join("debug/atpkg-abrew");
    std::fs::write(&copy, b"the bin the tests ran").expect("its copy");
    let at = std::fs::metadata(&ran)
        .and_then(|m| m.modified())
        .expect("written");
    std::fs::File::options()
        .write(true)
        .open(&copy)
        .and_then(|f| f.set_modified(at))
        .expect("its copy's time");
    let id = "path+file:///w/aterm/crates/atpkg#0.155.0";
    let artifact = |file: &std::path::Path, test: bool| {
        aterm_json::json!({
            "reason": "compiler-artifact",
            "package_id": id,
            "target": {"kind": [if test { "lib" } else { "bin" }], "name": "atpkg"},
            "profile": {"test": test},
            "filenames": [file.display().to_string()],
            "executable": file.display().to_string(),
            "fresh": true,
        })
        .to_string()
    };
    let wire = [
        artifact(&out(test).join(format!("atpkg-{test}")), true),
        artifact(&copy, false),
        aterm_json::json!({
            "reason": "build-script-executed",
            "package_id": id,
            "out_dir": out(script).display().to_string(),
        })
        .to_string(),
    ]
    .join("\n");
    let targo = tmp.join("bin/targo");
    crate::standin_body(
        &targo,
        &format!("/bin/cat '{}'", tmp.join("wire").display()),
    );
    let compiled = |wire: &str| {
        std::fs::write(tmp.join("wire"), wire).expect("the wire");
        let mut c = Ctx::new(
            tmp.clone(),
            Scope::workspace(),
            EnvSnapshot {
                cargo_target_dir: Some(target.clone()),
                ..EnvSnapshot::default()
            },
            tmp.join("scratch"),
        )
        .with_checkers_in_tests(true);
        c.tools.targo = targo.clone();
        c.main_build_dir = Some(format!("build.build-dir='{}'", leaf.display()));
        let spec = crate::plan::declared(&c.scope).remove(0);
        let mut r = Report::new(spec.title.clone());
        test_compile(&c, &spec, &mut r);
        assert!(
            r.outcomes().all(|(o, _)| o == crate::Outcome::Ok),
            "{}",
            r.render()
        );
        c
    };
    let record = leaf.join(atpkg::build_cache::stale::USED);
    let c = compiled("");
    assert!(crate::disk::keep_used(&c).is_empty(), "it reported nothing");
    assert!(!record.exists());

    let c = compiled(&wire);
    let said = crate::disk::keep_used(&c);
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(
        (&said[0].of, said[0].dir.as_path(), said[0].retired),
        (&crate::disk::Dir::Main, leaf.as_path(), 0),
        "{said:?}"
    );
    let kept = atpkg::build_cache::stale::Kept::decode(&std::fs::read(&record).expect("kept"))
        .expect("a record");
    let kept = &kept.runs[&crate::disk::run_key(&c)];
    assert_eq!(
        kept.profiles.get("debug").cloned().unwrap_or_default(),
        [test, bin, script].map(str::to_owned).into()
    );
    assert_eq!(kept.members, ["atpkg".to_owned()].into());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn no_targo_is_fail_closed_at_the_test_compile_and_honest_everywhere_after() {
    let c = ctx(Scope::workspace());
    assert!(!c.tools.have_targo());

    let mut r = Report::new("test compile");
    let spec = crate::plan::declared(&c.scope).remove(0);
    test_compile(&c, &spec, &mut r);
    let (outcome, label) = r.outcomes().next().expect("a decision");
    assert_eq!(
        outcome,
        crate::Outcome::Fail(crate::Severity::CouldNotRun),
        "{label}"
    );

    let dependent: [fn(&Ctx, &mut Report); 9] = [
        test,
        measuring_tests,
        measure_prebuild,
        doctests,
        spec_check,
        spec_checkers,
        export,
        libc_oracle,
        redraw_conformance,
    ];
    for stage in dependent {
        let mut r = Report::new("s");
        stage(&c, &mut r);
        for (outcome, label) in r.outcomes() {
            assert_eq!(outcome, crate::Outcome::Skip, "{label}");
        }
    }
}

#[cfg(unix)]
#[test]
fn checkers_do_not_retry_a_failed_main_test_compile() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let mut c = Ctx::new(
        temp.path().into(),
        Scope::workspace(),
        EnvSnapshot::default(),
        temp.path().join("scratch"),
    );
    c.tools.targo = "/usr/bin/false".into();
    c.tools.refused = None;
    let mut compile = Report::new("checkers compile");
    test_compile(&c, &crate::plan::declared(&c.scope).remove(0), &mut compile);
    assert!(
        c.test_compile_failed
            .load(std::sync::atomic::Ordering::Acquire)
    );
    c.scratch = temp.path().join("runtime-must-not-start");
    let mut runtime = Report::new("checkers");
    spec_checkers(&c, &mut runtime);
    assert!(
        !c.scratch.exists(),
        "a recording pass would create the scratch directory"
    );
    assert!(
        runtime
            .outcomes()
            .all(|(outcome, _)| outcome == Outcome::Fail(Severity::CouldNotRun))
    );
}

#[test]
fn the_spec_check_runs_whole_under_release_and_names_a_missing_checker() {
    let c = ctx(Scope::workspace());
    let tail = |cmd: &Cmd| {
        cmd.argv()[1..]
            .iter()
            .skip_while(|a| *a != "spec-check")
            .cloned()
            .collect::<Vec<_>>()
    };
    let dev = spec_check_cmd(&c);
    assert_eq!(tail(&dev), ["spec-check"]);
    let argv = dev.argv();
    assert!(
        argv.windows(2).any(|w| w == ["-p", "aterm-models"])
            && !argv.iter().any(|a| a == "--workspace"),
        "a run keeps its package; `targo run` refuses --workspace: {argv:?}"
    );
    assert_eq!(
        env_of(&dev, "CARGO_TARGET_DIR"),
        Some(c.target_dir.as_os_str().to_owned()),
        "the checker bootstrap uses the captured main target"
    );
    let release = spec_check_cmd(&ctx(Scope::workspace()).with_release(true));
    assert_eq!(tail(&release), ["spec-check", "--", "--full"]);
    let run = |code: Option<i32>, output: &str| exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: code == Some(0),
        output: output.to_string(),
        code,
        spawn_error: None,
        usage: None,
    };
    let missing = run(
        Some(3),
        "spec-check: could not run `models`: it needs Trust `ty`, which was not found. \
         Install it: aterm pkg install ty\n",
    );
    assert_eq!(
        libc_oracle_outcome(&missing),
        Outcome::Fail(Severity::CouldNotRun)
    );
    assert_eq!(
        spec_check_not_run_reason(&missing).as_deref(),
        Some(
            "could not run `models`: it needs Trust `ty`, which was not found. Install it: \
             aterm pkg install ty"
        )
    );
    let input = run(
        Some(3),
        "skip  ay           0.0s  could not run: no proofs/ay\n\nspec-check: COULD NOT RUN ay\n",
    );
    assert_eq!(
        spec_check_not_run_reason(&input).as_deref(),
        Some("no proofs/ay")
    );
    assert_eq!(
        libc_oracle_outcome(&run(Some(1), "")),
        Outcome::Fail(Severity::GateFailed)
    );
    assert_eq!(libc_oracle_outcome(&run(Some(0), "")), Outcome::Ok);
}

#[test]
fn the_export_check_reads_ship_checks_exit_code() {
    let c = ctx(Scope::workspace());
    let cmd = export_cmd(&c);
    assert_eq!(cmd.term_grace, Some(EXPORT_TERM_GRACE));
    assert_eq!(
        cmd.argv(),
        main_targo(&c, ship_args(["check", "--export"])).argv(),
        "export uses the main target's build-dir and test graph"
    );
    assert_eq!(
        env_of(&cmd, "CARGO_TARGET_DIR"),
        Some(c.target_dir.as_os_str().to_owned()),
        "the export bootstrap uses the captured main target"
    );
    assert_eq!(export_outcome(Some(0), "").0, Outcome::Ok);
    for code in [1, 2, 101] {
        assert_eq!(
            export_outcome(Some(code), "").0,
            Outcome::Fail(Severity::GateFailed),
            "{code}"
        );
    }
    assert_eq!(export_outcome(Some(3), "").0, Outcome::Skip);
    for code in [Some(130), None] {
        assert_eq!(
            export_outcome(code, "").0,
            Outcome::Fail(Severity::CouldNotRun)
        );
    }

    #[cfg(unix)]
    {
        use aterm_census::export::mainline;
        let dir = aterm_tempfile::tempdir().expect("private Git fixture");
        let root = dir.path();
        let repo = root.join("repo");
        let origin = root.join("origin.git");
        std::fs::create_dir(&repo).unwrap();
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let mut cmd = aterm_uds::fixture_git::git();
            cmd.current_dir(cwd)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args);
            let out = aterm_uds::deadline::output_within(&mut cmd, mainline::READ_WAIT).unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        git(
            root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "main"]);
        git(&repo, &["push", "-q", "origin", "HEAD:main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        let main = git(&repo, &["rev-parse", "HEAD"]);
        let scratch = root.join("capture");
        std::fs::create_dir(&scratch).unwrap();
        let mut c = Ctx::new(
            repo.clone(),
            Scope::workspace(),
            EnvSnapshot::default(),
            scratch,
        );
        c.path_env = std::env::var_os("PATH").unwrap_or_default();
        c.tools.targo = PathBuf::from("/bin/sh");
        c.tools.refused = None;
        c.tools.lock_refusal = None;
        let starts = std::cell::Cell::new(0);
        let producer = |_: &Cmd| {
            starts.set(starts.get() + 1);
            exec::run(
                &Cmd::new("/bin/sh").args(["-c", "printf 'export producer ran\\n'"]),
                c.exec_env(),
            )
        };
        let mut ready = Report::new("mainline export");
        export_with(&c, &mut ready, producer);
        assert_eq!(starts.get(), 1, "the eligible export producer actually ran");
        assert_eq!(ready.outcomes().next().unwrap().0, Outcome::Ok);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "feature"]);
        git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/feature"]);
        git(&repo, &["fetch", "-q", "origin"]);
        assert!(matches!(
            export_mainline(&c),
            Err(mainline::Refusal::NotOnMain)
        ));
        let mut feature = Report::new("pushed feature export");
        export_with(&c, &mut feature, producer);
        assert_eq!(starts.get(), 1, "a pushed feature branch starts no build");
        assert_eq!(feature.outcomes().next().unwrap().0, Outcome::Skip);
        git(&repo, &["push", "-q", "origin", "HEAD:main"]);
        git(&repo, &["update-ref", mainline::REF, &main]);
        assert!(
            matches!(export_mainline(&c), Err(mainline::Refusal::NotOnMain)),
            "only the locally fetched ref qualifies"
        );
        git(&repo, &["fetch", "-q", "origin"]);
        let mut fetched = Report::new("fetched mainline export");
        export_with(&c, &mut fetched, producer);
        assert_eq!(starts.get(), 2);
        assert_eq!(fetched.outcomes().next().unwrap().0, Outcome::Ok);
        git(&repo, &["update-ref", "-d", mainline::REF]);
        assert!(
            matches!(export_mainline(&c), Err(mainline::Refusal::Unread(_))),
            "an absent ref is not a nonancestor finding"
        );
        let mut unread = Report::new("unread mainline export");
        export_with(&c, &mut unread, producer);
        assert_eq!(starts.get(), 2, "unread ancestry starts no build");
        assert_eq!(unread.outcomes().next().unwrap().0, Outcome::Skip);
    }
}

#[test]
fn appkit_builds_and_drives_from_the_main_target_with_no_feature() {
    let a = redraw_conformance_build_args();
    assert!(!a.iter().any(|x| x == "--features"), "{a:?}");
    let bin = a.iter().position(|x| x == "--bin").expect("a --bin");
    assert_eq!(a[bin + 1], REDRAW_CONFORMANCE_BIN);
    assert!(a.contains(&"aterm-gui".to_string()));
    let c = ctx(Scope::workspace());
    assert_eq!(
        main_bin(&c, REDRAW_CONFORMANCE_BIN),
        PathBuf::from("/repo/target/.ubd/main.noindex/debug/aterm-redraw-conformance")
    );
    assert_eq!(
        main_example(&c, OBJC_DRIVES[0].example),
        PathBuf::from("/repo/target/.ubd/main.noindex/debug/examples/objc_live_class_audit")
    );
    let build = main_build_cmd(&c, objc_drives_build_args());
    assert_eq!(
        env_of(&build, "CARGO_TARGET_DIR"),
        Some(c.target_dir.as_os_str().to_owned()),
        "the driver build uses the captured main target"
    );
}

/// Every hop still invokes the build. An exact executable-and-runtime proof skips the
/// windowed child; a context mismatch drives it in the stable empty private runtime, never the
/// checkout. The command handed to the spawn path has a finalized, exactly empty environment.
#[cfg(unix)]
#[test]
fn appkit_group_carry_builds_then_skips_only_an_exact_windowed_driver() {
    const CONTEXT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for carried in [true, false] {
        let root = std::fs::canonicalize(
            crate::mktemp_dir(if carried {
                "atv-appkit-carried"
            } else {
                "atv-appkit-ran"
            })
            .expect("root"),
        )
        .expect("physical root");
        let target = root.join("target");
        let scratch = root.join("scratch");
        std::fs::create_dir_all(target.join("debug")).expect("target");
        std::fs::create_dir(&scratch).expect("scratch");
        let targo = root.join("bin/targo");
        let built = root.join("build-ran");
        let driven = root.join("driver-ran");
        let cwd_seen = root.join("driver-cwd");
        crate::standin_body(&targo, &format!("printf built >> '{}'", built.display()));
        let binary = target.join("debug").join(REDRAW_CONFORMANCE_BIN);
        crate::standin_body(
            &binary,
            &format!(
                "printf driven >> '{}'; printf '%s' \"$PWD\" > '{}'; exit 0",
                driven.display(),
                cwd_seen.display()
            ),
        );
        let mut c = Ctx::new(
            root.clone(),
            Scope::workspace(),
            EnvSnapshot {
                cargo_target_dir: Some(target),
                ..EnvSnapshot::default()
            },
            scratch.clone(),
        )
        .with_carry_context(Some(CONTEXT.to_owned()));
        c.tools.targo = targo;
        let paths = [(REDRAW_CONFORMANCE_BIN, binary.clone())];
        let mut prepared = Report::new("prepare exact prior AppKit runtime");
        let prior_runtime = appkit_group_runtime(
            &c,
            &mut prepared,
            crate::appkit_content::REDRAW_GROUP,
            &paths,
        )
        .expect("prior runtime");
        assert!(prior_runtime.commands[0].exact_env);
        assert!(prior_runtime.commands[0].envs.is_empty());
        assert!(prior_runtime.commands[0].runtime_environment_is_finalized());
        let prior_context = if carried {
            CONTEXT.to_owned()
        } else {
            "b".repeat(64)
        };
        let prior = crate::appkit_content::GroupProof::from_paths_and_commands(
            crate::appkit_content::REDRAW_GROUP,
            &prior_context,
            &paths,
            &prior_runtime.commands,
        )
        .expect("prior proof");
        let runtime_cwd = prior_runtime.cwd.clone();
        if carried {
            let cwd_drift_path = root.join("candidate-cwd-drift");
            std::fs::create_dir(&cwd_drift_path).expect("cwd drift");
            let cwd_drift = finalized_appkit_command(&c, &binary, &cwd_drift_path, Vec::new())
                .expect("finalized cwd drift");
            let cwd_drift = crate::appkit_content::GroupProof::from_paths_and_commands(
                crate::appkit_content::REDRAW_GROUP,
                CONTEXT,
                &paths,
                &[cwd_drift],
            )
            .expect("cwd-drift proof");
            assert_ne!(prior.content_key, cwd_drift.content_key);
            std::fs::remove_dir(&cwd_drift_path).expect("remove cwd drift");

            let env_drift = finalized_appkit_command(
                &c,
                &binary,
                &runtime_cwd,
                vec![("HOME".into(), "/different".into())],
            )
            .expect("finalized environment drift");
            let env_drift = crate::appkit_content::GroupProof::from_paths_and_commands(
                crate::appkit_content::REDRAW_GROUP,
                CONTEXT,
                &paths,
                &[env_drift],
            )
            .expect("environment-drift proof");
            assert_ne!(prior.content_key, env_drift.content_key);
        }
        drop(prior_runtime);
        // A crashed earlier run's private residue is removed before the runtime is re-keyed.
        aterm_uds::private_file::create_private_dir(&runtime_cwd).expect("stale cwd");
        std::fs::write(runtime_cwd.join("stale"), b"not an input").expect("stale entry");
        c.passed_appkit_groups = Some(BTreeMap::from([(prior.group.clone(), prior)]));
        let mut report = Report::new("redraw conformance");
        redraw_conformance(&c, &mut report);

        assert!(built.exists(), "the headless build always ran");
        assert_eq!(driven.exists(), !carried, "windowed child execution");
        assert!(
            report.outcomes().all(|(outcome, _)| outcome == Outcome::Ok),
            "{}",
            report.render()
        );
        if carried {
            assert!(report.render().contains("carried prior ok"));
            assert!(!cwd_seen.exists());
        } else {
            assert_eq!(
                std::fs::read_to_string(&cwd_seen).expect("driver cwd"),
                runtime_cwd.display().to_string()
            );
        }
        assert!(!runtime_cwd.exists(), "the stable group cwd is cleaned");
        std::fs::remove_dir_all(root).ok();
    }
}

#[cfg(unix)]
#[test]
fn appkit_group_replacement_after_a_drive_fails_closed() {
    let root = crate::mktemp_dir("atv-appkit-replaced").expect("root");
    let scratch = root.join("scratch");
    std::fs::create_dir(&scratch).expect("scratch");
    let binary = root.join(REDRAW_CONFORMANCE_BIN);
    std::fs::write(&binary, b"before").expect("binary");
    let c = Ctx::new(
        root.clone(),
        Scope::workspace(),
        EnvSnapshot::default(),
        scratch,
    )
    .with_carry_context(Some("a".repeat(64)));
    let paths = [(REDRAW_CONFORMANCE_BIN, binary.clone())];
    let mut prepared = Report::new("prepare exact AppKit runtime");
    let runtime = appkit_group_runtime(
        &c,
        &mut prepared,
        crate::appkit_content::REDRAW_GROUP,
        &paths,
    )
    .expect("runtime");
    let before = appkit_group_proof(&c, crate::appkit_content::REDRAW_GROUP, &paths, &runtime);
    assert!(before.is_some());
    std::fs::write(&binary, b"after").expect("replace");
    let mut report = Report::new("redraw conformance");
    finish_appkit_group(
        &c,
        &mut report,
        crate::appkit_content::REDRAW_GROUP,
        &paths,
        &runtime,
        before,
    );
    assert_eq!(
        report.outcomes().next().map(|(outcome, _)| outcome),
        Some(Outcome::Fail(Severity::CouldNotRun))
    );
    assert!(report.render().contains("changed while the group ran"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn objc_build_selects_every_driver_once_in_one_invocation() {
    let args = objc_drives_build_args();
    let examples: Vec<_> = args
        .windows(2)
        .filter(|pair| pair[0] == "--example")
        .map(|pair| pair[1].as_str())
        .collect();
    assert_eq!(examples, OBJC_DRIVES.map(|drive| drive.example));
    assert_eq!(args.iter().filter(|arg| *arg == "build").count(), 1);
    assert!(
        !args
            .iter()
            .any(|arg| arg == "--examples" || arg == "--features")
    );
}

#[cfg(unix)]
#[test]
fn native_rows_keep_results_and_report_only_their_cpu_increment() {
    let c = ctx(Scope::workspace());
    let meter = crate::memory::Meter::new();
    let member = |cpu_ns| aterm_sysprobe::MemberUsage {
        pid: 7,
        start: 1,
        cpu_ns,
        ..Default::default()
    };
    meter.book(7, &[member(2_000_000_000)]);
    let _held = crate::memory::metering(Some(meter.clone()));
    let mut report = Report::new("native fixture");
    let result = native_row(&c, &mut report, "planted failure", |r| {
        meter.book(7, &[member(5_000_000_000)]);
        r.fail("negative control");
        17
    });
    assert_eq!(result, 17);
    assert!(
        report
            .outcomes()
            .any(|(o, _)| o == Outcome::Fail(Severity::GateFailed))
    );
    let rows = |report: &Report| {
        report
            .render()
            .lines()
            .filter_map(|line| line.strip_prefix("verify-event: "))
            .map(|json| aterm_json::from_str::<aterm_json::Value>(json).unwrap())
            .collect::<Vec<_>>()
    };
    let events = rows(&report);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["event"].as_str(), Some("row-started"));
    assert_eq!(events[1]["event"].as_str(), Some("row-finished"));
    assert_eq!(events[1]["row"].as_str(), Some("planted failure"));
    assert_eq!(events[1]["cpu_s"].as_f64(), Some(3.0));
    let _unmetered = crate::memory::metering(None);
    let mut report = Report::new("unmetered fixture");
    native_row(&c, &mut report, "unread", |_| ());
    assert!(rows(&report)[1]["cpu_s"].is_null());
}

#[cfg(unix)]
#[test]
fn a_locked_console_skips_native_drives_but_an_unlocked_one_keeps_findings() {
    use crate::checks::Check;
    let base = || {
        let mut c = ctx(Scope::workspace()).with_checks(&[Check::Appkit]);
        // A real executable that always fails: reaching the ordinary build must remain red.
        c.tools.targo = PathBuf::from("/usr/bin/false");
        c.tools.refused = None;
        c.tools.lock_refusal = None;
        c
    };
    let locked = base().with_screen_lock_source(|| true);
    for spec in crate::plan::plan(&locked) {
        let result = run_stage(&locked, &spec);
        assert!(
            result
                .outcomes()
                .all(|(outcome, _)| outcome == Outcome::Skip)
        );
    }
    // Each skipped stage builds into the main lane's build directory when it runs, and no word
    // of the run's selection says the console was locked: the run's record of that directory
    // is kept under a scope of its own, so it retires nothing an unlocked run's stages built.
    assert_ne!(
        crate::disk::run_key(&locked),
        crate::disk::run_key(&base()),
        "a run that skipped a stage that builds is no run of the same scope"
    );
    let unlocked = base().with_screen_lock_source(|| false);
    let spec = crate::plan::plan(&unlocked)
        .into_iter()
        .find(|spec| spec.id == StageId::RedrawConformance)
        .unwrap();
    assert!(
        run_stage(&unlocked, &spec)
            .outcomes()
            .any(|(outcome, _)| matches!(outcome, Outcome::Fail(_)))
    );

    let headless = ctx(Scope::packages(&[]))
        .with_checks(&[Check::Test])
        .with_screen_lock_source(|| panic!("a headless stage must not probe the desktop"));
    for spec in crate::plan::plan(&headless) {
        let _ = run_stage(&headless, &spec);
    }
}

#[test]
fn no_windows_skips_each_windowed_stage_by_name_and_nothing_else() {
    use crate::plan::{WINDOWED, plan};
    let every: Vec<crate::checks::Check> = crate::checks::CHECKS.iter().map(|s| s.check).collect();
    let base = || {
        let mut c = ctx(Scope::workspace()).with_checks(&every);
        c.tools.targo = PathBuf::from("/nonexistent/targo");
        c
    };
    let plain = base();
    let quiet = base().with_no_windows(true);
    let planned = plan(&quiet);
    assert_eq!(planned, plan(&plain), "the flag removes no stage");
    let mut seen = Vec::new();
    for spec in &planned {
        let Some((_, name)) = WINDOWED.iter().find(|(id, _)| *id == spec.id) else {
            assert_eq!(spec.id.no_windows_skip(), None, "{:?}", spec.id);
            continue;
        };
        seen.push(spec.id);
        let r = run_stage(&quiet, spec);
        let rows: Vec<_> = r.outcomes().collect();
        assert_eq!(rows.len(), 1, "{:?}", spec.id);
        assert_eq!(rows[0].0, Outcome::Skip, "{:?}", spec.id);
        assert!(rows[0].1.starts_with(name), "{}", rows[0].1);
        let ran = run_stage(&plain, spec);
        assert!(
            ran.outcomes()
                .all(|(_, label)| !label.contains("--no-windows")),
            "{:?}: without the flag the stage's own body decides: {}",
            spec.id,
            ran.render()
        );
        assert!(ran.outcomes().next().is_some(), "{:?} decided", spec.id);
    }
    for (id, _) in WINDOWED {
        assert!(seen.contains(&id), "{id:?} is planned");
    }
}

/// NEGATIVE CONTROL: exit `2` (`NOT RUN`) is always `CouldNotRun` (never `Ok`), and when the
/// driver's transcript names `NOT RUN — <reason>` (such as a closed clamshell with no active
/// display), the ladder row quotes that reason directly instead of hiding it behind the
/// generic fallback.
#[test]
fn every_objc_drive_reads_its_exit_contract() {
    for drive in &OBJC_DRIVES {
        let name = drive.example;
        let judged = format!("{name}: stub\n\n{OBJC_VERDICT_HEADER}\n{name}: OK\n");
        let read = |code| objc_drive_outcome(drive, code, &judged);
        assert_eq!(read(Some(0)).0, Outcome::Ok, "{name}");
        assert_eq!(
            read(Some(1)).0,
            Outcome::Fail(Severity::GateFailed),
            "{name}"
        );
        assert_eq!(
            read(Some(2)).0,
            Outcome::Fail(Severity::CouldNotRun),
            "{name}"
        );
        let with_reason = objc_drive_outcome(
            drive,
            Some(2),
            &format!(
                "{name}: NOT RUN — no display is online for drawing (CoreGraphics lists no active display)\n"
            ),
        );
        assert_eq!(
            with_reason.0,
            Outcome::Fail(Severity::CouldNotRun),
            "{name}"
        );
        assert_eq!(
            with_reason.1,
            format!(
                "{name}: NOT RUN — no display is online for drawing (CoreGraphics lists no active display) (exit 2, never a pass)"
            ),
            "{name}"
        );
        let want = match drive.code3 {
            Code3::WeakerPassOk(_) => Outcome::Ok,
            Code3::Hung => Outcome::Fail(Severity::CouldNotRun),
            Code3::AbortFinding | Code3::None => Outcome::Fail(Severity::GateFailed),
        };
        assert_eq!(read(Some(3)).0, want, "{name}");
        for code in [4, 5, 101, 255] {
            assert_eq!(
                read(Some(code)).0,
                Outcome::Fail(Severity::GateFailed),
                "{name}: exit {code} must not be green"
            );
        }
        assert_eq!(read(None).0, Outcome::Fail(drive.no_status), "{name}");
    }
    for code3 in [Code3::Hung, Code3::AbortFinding, Code3::None] {
        assert!(OBJC_DRIVES.iter().any(|d| d.code3 == code3), "{code3:?}");
    }
}

#[test]
fn an_unregistered_objc_success_description_cannot_claim_a_pass() {
    let drive = ObjcDrive {
        example: "unregistered_drive",
        code3: Code3::None,
        no_status: Severity::CouldNotRun,
    };
    let judged = format!("{OBJC_VERDICT_HEADER}\nunregistered-drive: OK\n");
    let (outcome, why) = objc_drive_outcome(&drive, Some(0), &judged);
    assert_eq!(outcome, Outcome::Fail(Severity::GateFailed));
    assert!(why.contains("success-description table"), "{why}");
    assert_eq!(
        objc_drive_outcome(&drive, Some(2), "").0,
        Outcome::Fail(Severity::CouldNotRun)
    );
}

/// NEGATIVE CONTROL (KI-1110-3): a driver's verdict, not its exit status, decides a pass.
/// `objc_live_class_audit` printed a FINDING and then exited `0` from a queued `terminate:`
/// before its verdict; each half of that transcript alone is red here, for every driver.
#[test]
fn an_objc_drive_is_judged_by_its_verdict_not_its_exit_status() {
    for drive in &OBJC_DRIVES {
        let name = drive.example;
        let dash = name.replace('_', "-");
        let gate = Outcome::Fail(Severity::GateFailed);
        let ok_codes: Vec<i32> = std::iter::once(0)
            .chain(matches!(drive.code3, Code3::WeakerPassOk(_)).then_some(3))
            .collect();
        for code in ok_codes {
            let unjudged = "=== A. ROWS ===\n  -applicationShouldTerminate: = 0x0\n";
            let (got, why) = objc_drive_outcome(drive, Some(code), unjudged);
            assert_eq!(got, gate, "{name} exit {code} without a verdict: {why}");
            assert!(why.contains(OBJC_VERDICT_HEADER), "{why}");
            for finding in [
                "  FINDING: applicationShouldTerminate: answered 0x0".to_owned(),
                "    FAIL the window kept its title".to_owned(),
                format!("{dash}: FINDING — the helper panicked"),
                format!("{dash} terminate child: FAIL — LoopExiting arrived early"),
            ] {
                let t = format!("{finding}\n{OBJC_VERDICT_HEADER}\n{dash}: OK\n");
                let (got, why) = objc_drive_outcome(drive, Some(code), &t);
                assert_eq!(got, gate, "{name} exit {code} after {finding:?}: {why}");
                assert!(why.contains(finding.trim()), "{why}");
            }
        }
        // A finding outranks a could-not-run reading: it is proven either way.
        let t = format!("  FINDING: x\n{dash}: NOT RUN — no display\n");
        assert_eq!(
            objc_drive_outcome(drive, Some(2), &t).0,
            Outcome::Fail(Severity::GateFailed)
        );
        // Words that merely start alike are not findings.
        for benign in ["  FAILOVER held", "  FINDINGS none", "  ok   no FAIL here"] {
            let t = format!("{benign}\n{OBJC_VERDICT_HEADER}\n{dash}: OK\n");
            assert_eq!(
                objc_drive_outcome(drive, Some(0), &t).0,
                Outcome::Ok,
                "{benign}"
            );
        }
    }
}

#[test]
fn a_redraw_gate_that_could_not_run_is_never_a_pass_and_never_a_skip() {
    assert_eq!(
        redraw_outcome(Some(2)).0,
        Outcome::Fail(Severity::CouldNotRun)
    );

    assert_eq!(redraw_outcome(Some(0)).0, Outcome::Ok);
    assert_eq!(
        redraw_outcome(Some(1)).0,
        Outcome::Fail(Severity::GateFailed)
    );
    assert_eq!(
        redraw_outcome(Some(101)).0,
        Outcome::Fail(Severity::GateFailed)
    );
    assert_eq!(redraw_outcome(None).0, Outcome::Fail(Severity::CouldNotRun));
    for code in [None, Some(1), Some(2), Some(101), Some(-1)] {
        assert_ne!(redraw_outcome(code).0, Outcome::Ok, "{code:?}");
    }
}

fn measured_result(
    target: &str,
    planned: usize,
    passed: usize,
    failed: usize,
    refused: usize,
) -> String {
    format!(
        "measure result: {target} planned={planned} passed={passed} \
         failed={failed} refused={refused} unanswered=0 undecided=0\n"
    )
}

fn measured_subject(own: bool) -> Subject {
    Subject {
        name: "aterm 0.137.0 (22cb722a2ec9) at /Applications/aterm.app".into(),
        head: Some("215fb53331abd9acb2a33e7d7a21d1fa3c2bd8f2".into()),
        own,
    }
}

/// Every ladder row `decide_measuring_child` records for `output`.
fn decide_measured(ok: bool, output: String, subject: &Subject) -> Vec<(Outcome, String)> {
    let run = exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok,
        output,
        code: Some(if ok { 0 } else { 101 }),
        spawn_error: None,
        usage: None,
    };
    let mut r = Report::new("measuring");
    decide_measuring_child(&mut r, &run, "measuring matrices", subject);
    r.outcomes().map(|(o, l)| (o, l.to_string())).collect()
}

#[test]
fn a_measuring_child_requires_complete_decided_matrices() {
    let result = measured_result;
    let paint = result("paint", 23, 23, 0, 0);
    let spin =
        result("spin", 2, 2, 0, 0) + &result("aos", 1, 1, 0, 0) + &result("rewind", 2, 2, 0, 0);
    let decide = |ok: bool, output: String| {
        decide_measured(ok, output, &measured_subject(true))
            .into_iter()
            .next()
            .expect("a decision")
    };
    let could_not_run = Outcome::Fail(Severity::CouldNotRun);
    for output in [
        String::new(),
        "test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured\n".into(),
        result("paint", 0, 0, 0, 0) + &spin,
        result("paint", 23, 22, 0, 0) + &spin,
        result("paint", 23, 24, 0, 0) + &spin,
        paint.clone(),
        spin.clone(),
        paint.clone() + &paint + &spin,
        paint.replace("passed=23", "passed=unknown") + &spin,
        paint.clone() + &spin + "measure result: spin planned=2\n",
        paint.clone() + &result("spin", 2, 2, 0, 0),
        result("paint", usize::MAX, usize::MAX, 0, 1) + &spin,
    ] {
        assert_eq!(decide(true, output.clone()).0, could_not_run, "{output}");
    }
    let (outcome, label) = decide(true, spin.clone());
    assert_eq!(outcome, could_not_run);
    assert!(
        label.contains("paint") && !label.contains("spin"),
        "{label}"
    );
    assert_eq!(decide(true, paint.clone() + &spin).0, Outcome::Ok);
    assert_eq!(
        decide(false, result("paint", 23, 22, 0, 1) + &spin).0,
        could_not_run,
        "a row that could not run is not a finding about paint"
    );
    let undecided = |rows: &str| -> String {
        rows.split(',')
            .map(|row| {
                format!(
                    "measure row: rewind::{row} UNDECIDED\n\
                     REWIND COST UNPROVED: the interval holds the bound\n"
                )
            })
            .collect()
    };
    let spread = |passed: usize, undecided: usize| {
        format!(
            "measure result: rewind planned=2 passed={passed} failed=0 refused=0 \
             unanswered=0 undecided={undecided}\n"
        )
    };
    let base = paint.clone() + &result("spin", 2, 2, 0, 0) + &result("aos", 1, 1, 0, 0);
    let one = undecided("cost") + &spread(1, 1);
    let (outcome, label) = decide(true, base.clone() + &one);
    assert_eq!(
        outcome,
        Outcome::Ok,
        "an undecided row beside passes is stated, not a refusal"
    );
    assert!(
        label.ends_with(
            "undecided, stated and not a finding: rewind::cost \
             (REWIND COST UNPROVED: the interval holds the bound)"
        ),
        "{label}"
    );
    assert_eq!(
        decide(false, base.clone() + &one).0,
        Outcome::Fail(Severity::GateFailed),
        "the runner exits 0 there, so a crash after the summaries still fails"
    );
    let all_undecided = result("paint", 23, 0, 0, 0).replace("undecided=0", "undecided=23")
        + &result("spin", 2, 0, 0, 0).replace("undecided=0", "undecided=2")
        + &result("aos", 1, 0, 0, 0).replace("undecided=0", "undecided=1")
        + &undecided("cost,steps")
        + &spread(0, 2);
    let (outcome, label) = decide(true, all_undecided.clone());
    assert_eq!(
        outcome,
        Outcome::Ok,
        "a wholly undecided matrix on a busy machine is stated, never a refusal"
    );
    assert!(label.contains("rewind::steps"), "{label}");
    assert_eq!(
        decide(false, all_undecided).0,
        Outcome::Fail(Severity::GateFailed),
        "the runner exits 0 there, so a nonzero exit after the summaries still fails"
    );
    let loaded = "measure paint: unqualified — admitted loaded at its patience bound: CPU \
                  63.0% busy over 2.0s (quiet is under 25%), load 41.2 (quiet is at most \
                  28.0)\n";
    let all = base.clone() + &spread(2, 0);
    let (outcome, label) = decide(true, all.clone() + loaded);
    assert_eq!(
        outcome,
        Outcome::Ok,
        "a loaded matrix that failed nothing passes"
    );
    assert!(
        label.contains(
            "ran without a qualified quiet admission (stated, not a finding): paint — \
             admitted loaded at its patience bound"
        ),
        "{label}"
    );
    let (outcome, label) = decide(true, all.clone());
    assert_eq!(outcome, Outcome::Ok);
    assert!(!label.contains("qualified quiet"), "{label}");
    assert_eq!(
        decide(
            true,
            result("paint", 23, 22, 1, 0)
                + &result("spin", 2, 2, 0, 0)
                + &result("aos", 1, 1, 0, 0)
                + &spread(2, 0)
                + loaded
        )
        .0,
        Outcome::Fail(Severity::GateFailed),
        "a failed row on a loaded machine is still a finding"
    );
    let refused = "measure paint: COULD NOT RUN — cannot acquire its exclusive timing \
                   claim: timing not qualified: no CPU window formed at its patience bound\n";
    let rest = result("spin", 2, 2, 0, 0) + &result("aos", 1, 1, 0, 0) + &spread(2, 0);
    assert_eq!(
        decide(false, rest.clone() + refused).0,
        could_not_run,
        "a bench that refused its own admission could not run, never a finding"
    );
    assert_eq!(
        decide(false, rest.clone()).0,
        Outcome::Fail(Severity::GateFailed),
        "a missing summary with no refusal and a nonzero exit is the child's failure"
    );
    let crashed = result("spin", 2, 2, 0, 0) + &result("aos", 1, 1, 0, 0) + refused;
    assert_eq!(
        decide(false, crashed).0,
        Outcome::Fail(Severity::GateFailed),
        "one bench's refusal never hides another's crash"
    );
    assert_eq!(
        decide(
            false,
            result("paint", 23, 22, 1, 0)
                + &result("spin", 2, 2, 0, 0)
                + &result("aos", 1, 1, 0, 0)
                + &one
        )
        .0,
        Outcome::Fail(Severity::GateFailed),
        "a failed row outweighs an undecided one"
    );
    assert_eq!(
        decide(
            false,
            base.clone() + &result("rewind", 2, 0, 0, 1).replace("undecided=0", "undecided=1")
        )
        .0,
        could_not_run,
        "a row that could not run outweighs an undecided one"
    );
    assert_eq!(
        decide(
            false,
            result("paint", 23, 0, 0, 23)
                + &result("spin", 2, 0, 0, 2)
                + &result("aos", 1, 0, 0, 1)
                + &result("rewind", 2, 0, 0, 2)
        )
        .0,
        could_not_run,
        "neither matrix decided anything"
    );
    assert_eq!(
        decide(
            false,
            result("paint", 23, 22, 1, 0)
                + &result("spin", 2, 0, 0, 2)
                + &result("aos", 1, 0, 0, 1)
                + &result("rewind", 2, 0, 0, 2)
        )
        .0,
        Outcome::Fail(Severity::GateFailed),
        "a definite finding outweighs another matrix's refusal"
    );
    assert_eq!(
        decide(true, result("paint", 23, 22, 1, 0) + &spin).0,
        Outcome::Fail(Severity::GateFailed),
        "an incorrect zero exit cannot erase the row's finding"
    );
    assert_eq!(
        decide(false, paint + &spin).0,
        Outcome::Fail(Severity::GateFailed),
        "complete passes cannot hide an unexpected process failure"
    );
    assert_eq!(
        decide(
            false,
            "error: could not compile the measurement binary\n".into()
        )
        .0,
        Outcome::Fail(Severity::GateFailed)
    );
}

/// A row the subject has no answer to is a finding on HEAD's own build, and on another build
/// one skip line naming exactly that row, beside the verdict on the rows it answered.
#[test]
fn an_unanswered_row_fails_heads_build_and_is_named_on_another() {
    let paint = measured_result("paint", 23, 23, 0, 0);
    let spin = "measure row: spin::an_idle_open_rewind_view_arms_no_timer UNANSWERED\n\
                ctl rewind open: the subject does not know `rewind` (ERR unknown verb)\n\
                measure result: spin planned=3 passed=2 failed=0 refused=0 unanswered=1 undecided=0\n";
    let aos = "measure row: aos::a_repair UNANSWERED\naos repair named no main_us=\n\
               measure result: aos planned=1 passed=0 failed=0 refused=0 unanswered=1 undecided=0\n";
    let rewind = measured_result("rewind", 2, 2, 0, 0);
    let output = paint.clone()
        + spin
        + &bench_exit("spin", "exit status: 3")
        + aos
        + &bench_exit("aos", "exit status: 3")
        + &rewind;
    let named = "spin::an_idle_open_rewind_view_arms_no_timer (ctl rewind open: the subject \
                 does not know `rewind` (ERR unknown verb)); aos::a_repair (aos repair named \
                 no main_us=)";

    let other = decide_measured(false, output.clone(), &measured_subject(false));
    assert_eq!(other.len(), 2, "{other:?}");
    assert_eq!(other[0].0, Outcome::Ok, "{other:?}");
    assert_eq!(other[1].0, Outcome::Skip, "{other:?}");
    assert!(other[1].1.contains("(215fb53331ab)"), "{other:?}");
    assert!(
        other[1].1.ends_with(&format!("no answer to {named}")),
        "{other:?}"
    );

    let own = decide_measured(false, output.clone(), &measured_subject(true));
    assert_eq!(own.len(), 1, "{own:?}");
    assert_eq!(own[0].0, Outcome::Fail(Severity::GateFailed), "{own:?}");
    assert!(
        own[0].1.ends_with(&format!("no answer to {named}")),
        "{own:?}"
    );

    let failed = paint.replace("passed=23 failed=0", "passed=22 failed=1") + spin + aos + &rewind;
    let rows = decide_measured(false, failed, &measured_subject(false));
    assert_eq!(rows[0].0, Outcome::Fail(Severity::GateFailed), "{rows:?}");
    assert_eq!(rows[1].0, Outcome::Skip, "{rows:?}");
    let refused = paint.replace(
        "passed=23 failed=0 refused=0",
        "passed=22 failed=0 refused=1",
    ) + spin
        + aos
        + &rewind;
    let rows = decide_measured(false, refused, &measured_subject(false));
    assert_eq!(rows[0].0, Outcome::Fail(Severity::CouldNotRun), "{rows:?}");
    assert_eq!(rows[1].0, Outcome::Skip, "{rows:?}");
}

/// cargo's lines for a `--no-fail-fast` bench whose process ended `how`.
fn bench_exit(target: &str, how: &str) -> String {
    format!(
        "error: bench failed, to rerun pass `-p aterm-conformance --bench {target}`\n\n\
         Caused by:\n  process didn't exit successfully: \
         `/t/.ubd/build.noindex/test/deps/{target}-0123abcd --bench` ({how})\n"
    )
}

/// On a subject HEAD did not build, an unanswered row passes the rows it answered only when
/// the runner's own exit 3 on those matrices is every failure cargo names: a crash, a panic or
/// a kill after the summaries were printed is the child's failure, in any bench.
#[test]
fn an_unanswered_row_never_hides_a_crash_after_the_summaries() {
    let rewind = "measure row: rewind::a_rewind_step_is_shown_within_8ms UNANSWERED\n\
                  ctl metrics percentiles does not report n_rewind_shown\n\
                  measure result: rewind planned=2 passed=1 failed=0 refused=0 unanswered=1 undecided=0\n";
    let paint = measured_result("paint", 23, 23, 0, 0);
    let rest = measured_result("spin", 2, 2, 0, 0) + &measured_result("aos", 1, 1, 0, 0);
    let other = measured_subject(false);
    let first = |output: String| decide_measured(false, output, &other)[0].clone();
    let decided = |output: String| first(output).0;

    let alone = paint.clone() + &rest + rewind + &bench_exit("rewind", "exit status: 3");
    assert_eq!(
        decided(alone.clone()),
        Outcome::Ok,
        "the runner's own exit 3"
    );
    let rows = decide_measured(false, alone.clone(), &other);
    assert_eq!(rows[1].0, Outcome::Skip, "{rows:?}");
    assert!(rows[1].1.contains("n_rewind_shown"), "{rows:?}");
    let colored = alone.replace("error:", "\x1b[1m\x1b[91merror\x1b[0m:");
    assert_eq!(
        decided(colored),
        Outcome::Ok,
        "a colored cargo reads the same"
    );

    let segv = "signal: 11, SIGSEGV: invalid memory reference";
    for (output, why) in [
        (
            paint.clone()
                + &bench_exit("paint", segv)
                + &rest
                + rewind
                + &bench_exit("rewind", "exit status: 3"),
            "paint died of a signal after its summary",
        ),
        (
            paint.clone() + &rest + rewind + &bench_exit("rewind", segv),
            "the bench with the unanswered row died of a signal after its summary",
        ),
        (
            paint.clone()
                + "error: bench failed, to rerun pass `-p aterm-conformance --bench paint`\n"
                + &rest
                + rewind
                + &bench_exit("rewind", "exit status: 3"),
            "paint panicked after its summary (libtest's 101 has no `Caused by`)",
        ),
        (
            paint.clone() + &rest + rewind,
            "cargo named no failed bench, so its 101 is not the runner's",
        ),
        (
            paint.clone() + &rest + rewind + &bench_exit("rewind", "exit status: 1"),
            "another exit status",
        ),
        (
            paint.clone()
                + &bench_exit("paint", "exit status: 3")
                + &rest
                + rewind
                + &bench_exit("rewind", "exit status: 3"),
            "an exit 3 from a matrix that answered every row",
        ),
    ] {
        assert_eq!(
            decided(output.clone()),
            Outcome::Fail(Severity::GateFailed),
            "{why}: {output}"
        );
    }

    let killed = exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: alone,
        code: None,
        spawn_error: None,
        usage: None,
    };
    let mut r = Report::new("measuring");
    decide_measuring_child(&mut r, &killed, "measuring matrices", &other);
    assert_ne!(
        r.outcomes().next().map(|(o, _)| o),
        Some(Outcome::Ok),
        "a cargo that did not exit itself was killed"
    );
}

#[test]
fn a_callers_cargo_build_jobs_is_a_ceiling_for_the_side_lanes() {
    let lane = Lane::CellsTarget;
    let own = lane_build_jobs(lane);
    assert_eq!(lane_jobs(lane, None), own, "{lane:?}");
    assert_eq!(
        lane_jobs(lane, Some(1)),
        Some(1),
        "{lane:?}: a smaller ceiling wins"
    );
    assert_eq!(lane_jobs(lane, Some(1000)), own, "{lane:?}: never a floor");
    let ceiling = |raw: &str| {
        let mut c = ctx(Scope::workspace());
        c.env.cargo_build_jobs = Some(raw.into());
        c.build_jobs_ceiling()
    };
    assert_eq!(ceiling(" 3 "), Some(3));
    for junk in ["", "0", "-1", "four", "4.5"] {
        assert_eq!(ceiling(junk), None, "{junk:?}");
    }
    let mut c = ctx(Scope::workspace());
    c.env.cargo_build_jobs = Some("3".into());
    let envs = |cmd: &Cmd| -> Vec<(String, String)> {
        cmd.envs
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect()
    };
    let export = envs(&export_cmd(&c));
    assert!(!export.iter().any(|(name, _)| name == "CARGO_BUILD_JOBS"));
    assert_eq!(
        env_of(&export_cmd(&c), "CARGO_TARGET_DIR"),
        Some(c.target_dir.as_os_str().to_owned()),
        "the export bootstrap stays in the captured main target"
    );
}

/// The words of `argv` (its program first) before its subcommand and those after it up to
/// `--`, as cargo reads the line: a flag's value (`--config`, `-Z`, …) is no subcommand.
fn around_verb(argv: &[String]) -> (Vec<&str>, &str, Vec<&str>) {
    let mut words = argv[1..].iter().map(String::as_str);
    let mut before = Vec::new();
    while let Some(word) = words.next() {
        if matches!(
            word,
            "--config" | "-Z" | "--manifest-path" | "--target-dir" | "--color" | "-C"
        ) {
            before.push(word);
            before.extend(words.next());
        } else if word.starts_with('-') {
            before.push(word);
        } else {
            return (before, word, words.take_while(|w| *w != "--").collect());
        }
    }
    panic!("no subcommand: {argv:?}")
}

/// Every `--config` of a main-lane child, and of the test run's recording pass, goes before
/// its subcommand, the test graph's beside the build cache's and the recording runner's, and
/// the test graph's flag right after it: Trust's `targo` reads a `--config` after the verb in
/// place of those before it, which dropped the cache and the runner (every test binary then
/// ran in cargo's serial pass, unrecorded). NEGATIVE CONTROL: B16's line, its `--config`
/// after the verb, holds one there.
#[test]
fn a_main_lane_child_names_every_config_before_its_verb() {
    let mut c = ctx(Scope::crate_only("x"));
    c.main_build_dir = Some("build.build-dir='/cache'".to_string());
    let [compile, run] = test_cmds(&c, false);
    let recording = crate::testrun::recording_cmd(
        &run,
        "target.aarch64-apple-darwin.runner=[\"r\"]",
        "aarch64-apple-darwin",
    );
    let flag = aterm_toolchain::build_cache::TEST_GRAPH_FLAG;
    for cmd in [compile, run, recording, spec_check_cmd(&c)] {
        let argv = cmd.argv();
        let (before, _, after) = around_verb(&argv);
        assert!(!after.contains(&"--config"), "{argv:?}");
        assert_eq!(after.first(), Some(&flag), "{argv:?}");
        let configs = before.iter().filter(|w| **w == "--config").count();
        assert!(configs >= 2, "the cache's and the test graph's: {argv:?}");
    }
    let b16 = [
        "targo",
        "--config",
        "build.build-dir='/cache'",
        "--unverified",
        "test",
        flag,
        "--config",
        "resolver.feature-unification=\"workspace\"",
    ]
    .map(String::from);
    assert!(around_verb(&b16).2.contains(&"--config"));
}

#[test]
fn a_callers_local_config_keeps_the_main_lanes_cache_and_test_graph() {
    let mut c = ctx(Scope::crate_only("x"));
    c.main_build_dir = Some("build.build-dir='/cache'".to_string());
    let argv = main_targo(
        &c,
        ["--unverified", "test", "--config", "build.jobs=1"]
            .map(String::from)
            .to_vec(),
    )
    .argv();
    let (before, verb, after) = around_verb(&argv);
    assert_eq!(verb, "test");
    assert!(!before.contains(&"--config"), "{argv:?}");
    for value in [
        "build.build-dir='/cache'",
        aterm_toolchain::build_cache::TEST_GRAPH_CONFIG[1],
        "build.jobs=1",
    ] {
        assert!(after.contains(&value), "{argv:?}");
    }
}

#[test]
fn the_main_lanes_children_lead_with_the_shims_build_dir() {
    let mut c = ctx(Scope::workspace());
    let repo_target = std::path::absolute(&c.target_dir).expect("absolute");
    let fallback = format!(
        "build.build-dir='{}'",
        aterm_toolchain::build_cache::target_build_path(&repo_target).display()
    );
    assert_eq!(
        c.main_build_dir.as_deref(),
        Some(fallback.as_str()),
        "no HOME in the fixture: no shim cache"
    );
    assert!(
        c.child_env_add
            .iter()
            .any(|(k, v)| k == "CARGO_TARGET_DIR" && v == c.target_dir.as_os_str())
    );
    let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
    let unify = aterm_toolchain::build_cache::TEST_GRAPH_FLAG;
    let narrow = || {
        ["--unverified", "test", "-p", "x"]
            .map(String::from)
            .to_vec()
    };
    assert_eq!(
        main_targo(&c, narrow()).argv()[1..],
        [
            "--config",
            fallback.as_str(),
            "--unverified",
            config,
            workspace,
            "test",
            unify,
            "-p",
            "x"
        ]
    );
    let value = "build.build-dir='/h/Library/Caches/aterm/.ubd/cargo-build.noindex{workspace-root}/.cargo-build'";
    c.main_build_dir = Some(value.to_string());
    let [compile, run] = test_cmds(&c, false);
    let export = export_cmd(&c);
    for cmd in [
        &compile,
        &run,
        &spec_check_cmd(&c),
        &linux_lab_build_cmd(&c),
        &export,
    ] {
        assert_eq!(cmd.argv()[1..3], ["--config", value], "{:?}", cmd.argv());
        assert_eq!(
            (&cmd.argv()[4..6], &cmd.argv()[7]),
            (
                &[config, workspace].map(String::from)[..],
                &unify.to_owned()
            ),
            "the workspace's features, around the verb: {:?}",
            cmd.argv()
        );
        assert!(
            !cmd.envs
                .iter()
                .any(|(name, _)| name == aterm_toolchain::build_cache::BUILD_DIR_ENV),
            "a nested cargo in a build script must not inherit it: {:?}",
            cmd.envs
        );
    }
    assert!(
        main_targo(&c, vec!["tippy".into()])
            .envs
            .iter()
            .any(
                |(name, directory)| name == aterm_toolchain::build_cache::BUILD_DIR_ENV
                    && directory
                        == aterm_toolchain::build_cache::directory_from_config(value).unwrap()
            ),
        "the same placement must reach an external subcommand"
    );
    c.main_build_dir = None;
    assert_eq!(
        main_targo(&c, narrow()).argv()[1..],
        ["--unverified", config, workspace, "test", unify, "-p", "x"],
        "unplaced (off macOS, no shim), the main lane still resolves the workspace's features"
    );
    assert!(
        !main_targo(&c, vec!["tippy".into()])
            .envs
            .iter()
            .any(|(name, _)| name == aterm_toolchain::build_cache::BUILD_DIR_ENV)
    );
    assert_eq!(
        env_of(&export_cmd(&c), "CARGO_TARGET_DIR"),
        Some(c.target_dir.as_os_str().to_owned()),
        "the main target stays captured independently of intermediate placement"
    );
}

#[test]
fn the_codex_alab_proof_drives_the_main_targets_aterm() {
    let lane = |cmd: &Cmd| {
        cmd.envs
            .iter()
            .find(|(k, _)| k.to_str() == Some("CARGO_TARGET_DIR"))
            .map(|(_, v)| PathBuf::from(v))
    };
    let every: Vec<crate::checks::Check> = crate::checks::CHECKS.iter().map(|s| s.check).collect();
    let c = ctx(Scope::workspace()).with_checks(&every);
    let plan = crate::plan::plan(&c);
    let lane_of = |id: StageId| {
        let spec = plan
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("{id:?} is planned"));
        lane_dir(&c, spec.lane)
    };

    let aterm = codex_aterm_binary(&c);
    let build = codex_aterm_build_cmd(&c);
    let suite = codex_alab_proof_cmd(&c);
    let lead = [
        "--config".to_string(),
        c.main_build_dir.clone().expect("a build dir"),
    ];
    assert_eq!(build.argv()[1..3], lead);
    let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
    let unify = aterm_toolchain::build_cache::TEST_GRAPH_FLAG;
    assert_eq!(
        build.argv()[3..],
        [
            "--unverified",
            config,
            workspace,
            "build",
            unify,
            "--profile",
            "test",
            "-q",
            "-p",
            "aterm",
            "-p",
            "aterm-lab",
            "--bin",
            "aterm",
            "--bin",
            "aterm-lab"
        ]
    );
    assert!(build.demoted, "the build only compiles");
    assert!(!suite.demoted, "the suite runs code");
    assert_eq!(
        lane(&build),
        Some(c.target_dir.clone()),
        "the captured main target"
    );
    assert_eq!(lane_of(StageId::CodexAlabProof), Some(c.target_dir.clone()));
    assert_eq!(aterm, c.target_dir.join("debug/aterm"));
    assert_eq!(
        suite.argv()[1..],
        [
            "codex-alab-proof".to_string(),
            "--aterm".to_string(),
            aterm.display().to_string()
        ]
    );
    assert_eq!(suite.program, c.target_dir.join("debug/aterm-lab"));
    assert!(suite.envs.is_empty(), "{:?}", suite.envs);
    assert_eq!(
        suite.term_grace,
        Some(CODEX_TERM_GRACE),
        "a lane over the ceiling is sent SIGTERM first, so it tears down"
    );
}

#[test]
fn the_linux_lane_runs_the_main_targets_lab_and_its_not_run_is_never_a_pass() {
    use crate::Outcome;
    let c = ctx(Scope::workspace()).with_checks(&[crate::checks::Check::Linux]);
    let build = linux_lab_build_cmd(&c);
    let lead = [
        "--config".to_string(),
        c.main_build_dir.clone().expect("a build dir"),
    ];
    assert_eq!(build.argv()[1..3], lead);
    let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
    let unify = aterm_toolchain::build_cache::TEST_GRAPH_FLAG;
    assert_eq!(
        build.argv()[3..],
        [
            "--unverified",
            config,
            workspace,
            "build",
            unify,
            "--profile",
            "test",
            "-q",
            "-p",
            "aterm-lab",
            "--bin",
            "aterm-lab"
        ]
    );
    assert!(linux_lab_build_args().windows(2).any(|w| w == MAIN_PROFILE));
    assert_eq!(
        env_of(&build, "CARGO_TARGET_DIR"),
        Some(c.target_dir.as_os_str().to_owned()),
        "the lab build uses the captured main target"
    );
    assert!(build.demoted, "the build only compiles");
    let lane = linux_lane_cmd(&c);
    assert!(!lane.demoted, "the lane runs code");
    assert!(
        lane.term_grace.is_some(),
        "the lab stops its VM before the kill"
    );
    assert_eq!(lane.program, c.target_dir.join("debug/aterm-lab"));
    assert_eq!(lane.argv()[1..], ["linux".to_string(), "proof".to_string()]);
    let admitting = |mode| linux_lane_cmd(&ctx(Scope::workspace()).with_quiet(Some(mode)));
    assert_eq!(
        admitting(aterm_capacity::Admit::Now).argv()[1..],
        ["linux", "--admit", "now", "proof"].map(String::from),
        "the run's --admit now is the lab's"
    );
    assert_eq!(
        admitting(aterm_capacity::Admit::Wait).argv()[1..],
        ["linux", "proof"].map(String::from),
        "wait is the lab's own default"
    );
    assert_eq!(
        linux_lane_outcome(Some(0), ""),
        (Outcome::Ok, LINUX_LANE.to_string())
    );
    assert_eq!(
        linux_lane_outcome(Some(1), "").0,
        Outcome::Fail(Severity::GateFailed)
    );
    let transcript = "aterm-lab: NOT RUN: no limactl: LAB_LIMACTL=/path/to/limactl names \
                      one; get lima's official release from github.com/lima-vm/lima/releases\n";
    let (outcome, label) = linux_lane_outcome(Some(3), transcript);
    assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun));
    assert!(
        label.contains("no limactl: LAB_LIMACTL=/path/to/limactl names one"),
        "{label}"
    );
    assert_eq!(
        linux_lane_outcome(None, "").0,
        Outcome::Fail(Severity::CouldNotRun)
    );
    let verdict_over = |(outcome, label): (Outcome, String)| {
        let mut r = Report::new("linux");
        r.record(outcome, label);
        let result = crate::report::judge(
            crate::checks::Check::Linux,
            &[&r],
            1.0,
            "",
            String::new(),
            None,
        );
        crate::report::verdict(&[result], false, 1.0)
    };
    let (line, code) = verdict_over(linux_lane_outcome(Some(3), transcript));
    assert_eq!(code, crate::exit::COULD_NOT_RUN, "{line}");
    assert!(line.starts_with("verify: COULD NOT RUN — linux"), "{line}");
    let (line, code) = verdict_over(codex_alab_proof_outcome(Some(3), transcript));
    assert_eq!(code, crate::exit::PASS, "{line}");
}

#[test]
fn the_codex_alab_proof_without_a_package_is_a_named_skip_never_a_pass() {
    use crate::Outcome;
    assert_eq!(
        codex_alab_proof_outcome(Some(0), "PASS"),
        (Outcome::Ok, CODEX_ALAB_LANE.to_string())
    );
    assert_eq!(
        codex_alab_proof_outcome(Some(1), "FAIL: 2 check(s)").0,
        Outcome::Fail(Severity::GateFailed)
    );
    let transcript = "aterm-lab: NOT RUN: no codex-alab package (the managed codex: /x is not there; pass --package <dir>)\n";
    let (outcome, label) = codex_alab_proof_outcome(Some(3), transcript);
    assert_eq!(outcome, Outcome::Skip);
    assert!(label.contains("no codex-alab package"), "{label}");
    assert_eq!(
        codex_alab_proof_outcome(None, "").0,
        Outcome::Fail(Severity::CouldNotRun),
        "a signal decided nothing"
    );
    let (outcome, label) = codex_alab_proof_outcome(Some(3), "");
    assert_eq!(outcome, Outcome::Skip);
    assert!(label.contains("the lab gave no reason"), "{label}");
    for other in [2, 77, 101] {
        assert_eq!(
            codex_alab_proof_outcome(Some(other), "").0,
            Outcome::Fail(Severity::GateFailed),
            "exit {other}"
        );
    }
    let c = ctx(Scope::workspace());
    let cmd = codex_alab_proof_cmd(&c);
    assert_eq!(cmd.argv()[1], "codex-alab-proof");
    assert_eq!(cmd.argv()[3], codex_aterm_binary(&c).to_string_lossy());
    assert_eq!(
        std::path::Path::new(&cmd.argv()[0]).parent(),
        codex_aterm_binary(&c).parent()
    );
}

#[test]
fn manifest_required_features_strips_comments_and_handles_single_quotes() {
    let manifest = "[package]\nname = 'my-pkg'\n\
                    [[bin]]\nname = \"b\"\n\
                    required-features = [\n\
                      \"feat-a\", # a comment with ] inside\n\
                      'feat-b',\n\
                    ]\n";
    assert_eq!(
        manifest_required_features(manifest),
        vec![
            ("my-pkg".to_string(), "feat-a".to_string()),
            ("my-pkg".to_string(), "feat-b".to_string()),
        ]
    );
}
