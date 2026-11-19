// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selected readiness observations, and what a checked cell keeps of its directory, use the real
//! cell resolver and the real run dispatcher.

use super::*;
use crate::checks::Check;
use crate::plan::{StageId, StageSpec};
use crate::{EnvSnapshot, Scope, Seams, Show};

const TARGET: &str = "wasm32-unknown-unknown";

/// The fixture workspace and its context, with no targo: nothing here can resolve a cell. Only a
/// test that never resolves one takes it; every other takes [`world`].
fn ground() -> (aterm_tempfile::TempDir, Ctx) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver='2'\nmembers=['app','other']\n",
    )
    .unwrap();
    std::fs::write(root.join("Cargo.lock"), "version=4\n[[package]]\nname='app'\nversion='1.0.0'\n[[package]]\nname='other'\nversion='1.0.0'\n").unwrap();
    for name in ["app", "other"] {
        std::fs::create_dir_all(root.join(name).join("src")).unwrap();
        std::fs::write(root.join(name).join("src/lib.rs"), "").unwrap();
        std::fs::write(
            root.join(name).join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n"),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("app/build.rs"),
        "fn main() { panic!(\"resolution must not execute this\"); }",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("crates/aterm-gui")).unwrap();
    std::fs::write(root.join("crates/aterm-gui/Cargo.toml"), "").unwrap();
    let mut ctx = Ctx::new(
        root.to_owned(),
        Scope::crate_only("app"),
        EnvSnapshot::default(),
        root.join("scratch"),
    )
    .with_checks(&[Check::Lint, Check::Cells])
    .with_events(Some(
        crate::event_log::EventLog::open(&root.join("events")).unwrap(),
    ));
    ctx.path_env = std::env::var_os("PATH").unwrap_or_default();
    (dir, ctx)
}

/// [`ground`] whose unready cell the real resolver walks: `targo metadata` names the members and
/// the unit graph says whether the cell holds a selected crate, both run by the suite's own targo,
/// the `CARGO` that `targo test` sets. The fixture's empty [`EnvSnapshot`] discovers no targo, so
/// a run of this binary without `CARGO` stops here, not on a resolution that could not spawn (nor
/// on a plan it never made, which would queue a cold claim in the live capacity ledger).
fn world() -> (aterm_tempfile::TempDir, Ctx) {
    let (dir, mut ctx) = ground();
    ctx.tools.targo = std::env::var_os("CARGO")
        .expect("CARGO, the suite's own targo (`targo test` sets it), walks the fixture's cell")
        .into();
    (dir, ctx)
}

fn captured(ctx: &Ctx) -> Option<PreparedRun<'_>> {
    let installed = ctx.root.join("installed").exists();
    let clippy = !ctx.root.join("no-clippy").exists();
    Some(PreparedRun {
        ctx,
        inputs: Ok(Inputs {
            policy: Vec::new(),
            host: "fixture-host".into(),
            cells: vec![Cell {
                name: "fixture-browser".into(),
                triple: TARGET.into(),
                package: "app".into(),
            }],
            stock: Ok(rustup::Stock {
                toolchain: rustup::Toolchain {
                    name: "fixture-stock".into(),
                    cargo: "/must-not-compile".into(),
                    rustc: "/must-not-compile".into(),
                    clippy,
                },
                targets: if installed {
                    vec![TARGET.into()]
                } else {
                    Vec::new()
                },
            }),
            members: None,
            plans: None,
        }),
    })
}

fn observations(ctx: &Ctx, wanted: crate::event_log::EventKind) -> Vec<aterm_json::Value> {
    std::fs::read_to_string(ctx.root.join("events"))
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("verify-event: "))
        .filter_map(|json| match crate::event_log::observation(json) {
            crate::event_log::Observation::Known { kind, value } if kind == wanted => Some(value),
            _ => None,
        })
        .collect()
}

fn notes(ctx: &Ctx) -> Vec<aterm_json::Value> {
    observations(
        ctx,
        crate::event_log::EventKind::CellPrerequisiteUnavailable,
    )
}

fn native(ctx: &Ctx, spec: &StageSpec) -> Report {
    let observed = notes(ctx);
    if ctx.root.join("expect-note").exists() {
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0]["complete"].as_bool(), Some(false));
    } else {
        assert!(observed.is_empty());
    }
    std::fs::write(ctx.root.join("native-ran"), b"yes").unwrap();
    if ctx.root.join("install-during-native").exists() {
        std::fs::write(ctx.root.join("installed"), b"yes").unwrap();
    }
    if ctx.root.join("remove-manifest-during-native").exists() {
        std::fs::remove_file(ctx.root.join("app/Cargo.toml")).unwrap();
    }
    let mut report = Report::new(&spec.title);
    report.pass("the other check ran");
    report
}

fn run(ctx: &Ctx, prepare: Option<crate::PrepareCells>) -> (i32, String) {
    let mut out = Vec::new();
    let code = crate::run_with(
        ctx,
        &mut out,
        &mut std::io::sink(),
        &Show::default(),
        Seams {
            stage: native,
            prepare_cells: prepare,
            prepare_build_dir: |_| Ok(()),
            ..crate::LIVE
        },
    )
    .unwrap();
    (code, String::from_utf8(out).unwrap())
}

#[test]
fn selected_missing_std_or_clippy_is_observed_before_native_work_without_gating_it() {
    for clippy_missing in [false, true] {
        let (_dir, ctx) = world();
        std::fs::write(ctx.root.join("expect-note"), b"yes").unwrap();
        if clippy_missing {
            std::fs::write(ctx.root.join("installed"), b"yes").unwrap();
            std::fs::write(ctx.root.join("no-clippy"), b"yes").unwrap();
        }
        let (code, output) = run(&ctx, Some(captured));
        assert_eq!(code, crate::exit::COULD_NOT_RUN, "{output}");
        assert!(ctx.root.join("native-ran").exists());
        let observed = notes(&ctx);
        let detail = observed[0]["detail"].as_str().unwrap();
        assert!(detail.contains(if clippy_missing {
            "has no clippy"
        } else {
            "std"
        }));
        assert!(detail.contains(crate::rustup::PROVISION));
        assert!(output.contains("NO-TOOLCHAIN"), "{output}");
    }
}

#[test]
fn an_unselected_missing_target_is_not_a_missing_prerequisite() {
    let (_dir, mut ctx) = world();
    ctx.scope = Scope::crate_only("other");
    let (code, output) = run(&ctx, Some(captured));
    assert_eq!(code, crate::exit::PASS, "{output}");
    assert!(notes(&ctx).is_empty());
    assert!(ctx.root.join("native-ran").exists());
}

#[test]
fn captured_stock_and_terminal_resolution_survive_a_later_install_without_rewalking() {
    let (_dir, ctx) = world();
    for name in [
        "expect-note",
        "install-during-native",
        "remove-manifest-during-native",
    ] {
        std::fs::write(ctx.root.join(name), b"yes").unwrap();
    }
    let (code, output) = run(&ctx, Some(captured));
    assert_eq!(code, crate::exit::COULD_NOT_RUN, "{output}");
    assert!(ctx.root.join("installed").exists());
    assert!(!ctx.root.join("app/Cargo.toml").exists());
    assert!(output.contains("NO-TOOLCHAIN"), "{output}");
    assert!(!output.contains("UNRESOLVED"), "{output}");
}

#[test]
fn reusing_a_context_does_not_reuse_an_earlier_preparation() {
    let (_dir, mut ctx) = world();
    std::fs::write(ctx.root.join("expect-note"), b"yes").unwrap();
    let (code, _) = run(&ctx, Some(captured));
    assert_eq!(code, crate::exit::COULD_NOT_RUN);
    std::fs::remove_file(ctx.root.join("expect-note")).unwrap();
    std::fs::write(ctx.root.join("events"), b"").unwrap();
    ctx.scope = Scope::crate_only("other");
    let (code, output) = run(&ctx, Some(captured));
    assert_eq!(code, crate::exit::PASS, "{output}");
    assert!(notes(&ctx).is_empty());
}

#[test]
fn ready_inputs_resolve_once_and_unowed_cells_never_prepare() {
    let (_dir, mut ctx) = world();
    std::fs::write(ctx.root.join("installed"), b"yes").unwrap();
    {
        let mut prepared = captured(&ctx).unwrap();
        assert!(prepared.inputs.as_ref().unwrap().members.is_none());
        prepared.resolve();
        assert!(prepared.needs_compiler(&ctx).unwrap());
        std::fs::remove_file(ctx.root.join("app/Cargo.toml")).unwrap();
        prepared.resolve();
        assert!(matches!(
            &prepared.inputs.as_ref().unwrap().plans.as_ref().unwrap()[0].plan,
            CellPlan::Compile(_)
        ));
    }
    fn forbidden(_: &Ctx) -> Option<PreparedRun<'_>> {
        panic!("no cell preparation was owed");
    }
    ctx.checks = vec![Check::Lint];
    assert_eq!(run(&ctx, Some(forbidden)).0, crate::exit::PASS);
    ctx.checks = vec![Check::Lint, Check::Cells];
    assert_eq!(run(&ctx, None).0, crate::exit::PASS);
}

#[test]
fn a_prepared_shim_failure_is_not_relabelled_or_halted_early() {
    let (_dir, ctx) = world();
    let mut prepared = captured(&ctx).unwrap();
    prepared.inputs.as_mut().unwrap().policy.push(Shim {
        package: "missing-sys".into(),
        version: "1.0.0".into(),
        triple: TARGET.into(),
        links: "missing".into(),
    });
    prepared.resolve();
    assert!(prepared.unavailable().is_empty());
    assert!(
        matches!(&prepared.inputs.as_ref().unwrap().plans.as_ref().unwrap()[0].plan, CellPlan::Terminal(CellOutcome::Failed { status, .. }) if status == "SHIM-DEAD")
    );
    assert!(ctx.halted().is_none());
}

#[test]
fn a_later_halt_takes_precedence_over_the_cached_missing_tool() {
    let (_dir, mut ctx) = world();
    ctx.halt = crate::sched::Halt::at_first_red("fixture", |_| {});
    let mut prepared = captured(&ctx).unwrap();
    prepared.resolve();
    assert_eq!(prepared.unavailable().len(), 1);
    assert!(ctx.halt.red("another-check"));
    let mut report = Report::new("cells");
    run_prepared(&ctx, &mut report, Some(prepared));
    let text = report.render();
    assert!(text.contains("NOT-STARTED"), "{text}");
    assert!(!text.contains("NO-TOOLCHAIN"), "{text}");
}

#[test]
fn selected_cell_admission_refusal_defers_cells_but_other_checks_still_run() {
    use crate::memory::StageKey as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ADMISSIONS: AtomicUsize = AtomicUsize::new(0);
    fn refuse(
        _: aterm_capacity::Need,
        _: aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
        ADMISSIONS.fetch_add(1, Ordering::SeqCst);
        Err(aterm_capacity::Error::Ledger(std::io::Error::other(
            "fixture admission refused",
        )))
    }
    for ready in [false, true] {
        let (_dir, mut ctx) = world();
        ctx.checks = vec![Check::Census, Check::Cells];
        if ready {
            std::fs::write(ctx.root.join("installed"), b"yes").unwrap();
        } else {
            std::fs::write(ctx.root.join("expect-note"), b"yes").unwrap();
        }
        // Only the cells stage claims memory here. The pure census callback still runs.
        let book =
            crate::memory::Book::at(Some(ctx.root.join("memory")), Some(8 << 30), Some(8 << 30));
        let spec = crate::plan::declared(&ctx.scope)
            .into_iter()
            .find(|spec| spec.id == StageId::ForeignCells)
            .unwrap();
        book.keep(
            &spec,
            &crate::memory::Key::of_run(StageId::ForeignCells, &ctx),
            1 << 20,
        )
        .unwrap();
        ctx.memory = Some(book);
        ADMISSIONS.store(0, Ordering::SeqCst);
        let mut output = Vec::new();
        let code = crate::run_with(
            &ctx,
            &mut output,
            &mut std::io::sink(),
            &Show::default(),
            Seams {
                stage: native,
                prepare_cells: Some(captured),
                prepare_build_dir: |_| Ok(()),
                admit_memory: refuse,
                ..crate::LIVE
            },
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(code, crate::exit::COULD_NOT_RUN, "{output}");
        assert_eq!(ADMISSIONS.load(Ordering::SeqCst), 1, "{output}");
        assert_eq!(notes(&ctx).len(), usize::from(!ready));
        assert!(ctx.root.join("native-ran").exists());
        assert!(!output.contains("NO-TOOLCHAIN"), "{output}");
    }
}

/// The old admission-before-selection order reaches the refusing spy and makes cells red.
/// The production dispatcher must consume the already resolved empty plan instead, even if
/// the manifest disappears after planning, without claiming memory or disk for that work.
#[test]
fn empty_cell_plans_run_without_compiler_admission_and_are_consumed_without_rewalking() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ADMISSIONS: AtomicUsize = AtomicUsize::new(0);
    fn refuse(
        _: aterm_capacity::Need,
        _: aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
        ADMISSIONS.fetch_add(1, Ordering::SeqCst);
        Err(aterm_capacity::Error::Ledger(std::io::Error::other(
            "fixture refusal",
        )))
    }
    for ready in [false, true] {
        let (_dir, mut ctx) = world();
        ctx.scope = Scope::crate_only("other");
        ctx.checks = vec![Check::Census, Check::Cells];
        ctx.memory = Some(crate::memory::Book::at(
            Some(ctx.root.join("memory")),
            Some(64 << 30),
            Some(56 << 30),
        ));
        if ready {
            std::fs::write(ctx.root.join("installed"), b"yes").unwrap();
        }
        let seams = Seams {
            stage: native,
            prepare_cells: Some(captured),
            prepare_build_dir: |_| Ok(()),
            admit_memory: refuse,
            ..crate::LIVE
        };
        ADMISSIONS.store(0, Ordering::SeqCst);
        let invocation = crate::RunPlan::capture(&ctx, seams).expect("complete graph and plan");
        assert!(matches!(
            invocation
                .cells
                .as_ref()
                .unwrap()
                .inputs
                .as_ref()
                .unwrap()
                .plans
                .as_ref()
                .unwrap()
                .as_slice(),
            [PreparedCell {
                plan: CellPlan::Terminal(CellOutcome::Unselected),
                ..
            }]
        ));
        assert!(!invocation.has_compiler_stage());
        assert!(!invocation.builds(Check::Cells));
        assert!(crate::disk::run_planned(&invocation).unwrap().is_none());
        assert!(crate::turn::Prepared::capture(&invocation, None).is_none());
        std::fs::remove_file(ctx.root.join("app/Cargo.toml")).unwrap();
        let mut output = Vec::new();
        let code = crate::run_with_plan(
            &ctx,
            &mut output,
            &mut std::io::sink(),
            &Show::default(),
            seams,
            Some(invocation),
            None,
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(code, crate::exit::PASS, "{output}");
        assert_eq!(ADMISSIONS.load(Ordering::SeqCst), 0, "{output}");
        let finished = observations(&ctx, crate::event_log::EventKind::CellFinished);
        assert_eq!(finished.len(), 1, "{finished:?}");
        assert_eq!(finished[0]["cell"].as_str(), Some("fixture-browser"));
        assert_eq!(finished[0]["triple"].as_str(), Some(TARGET));
        assert!(finished[0]["calls"].as_array().unwrap().is_empty());
        assert!(ctx.root.join("native-ran").exists());
        assert!(notes(&ctx).is_empty());
        assert!(
            !ctx.root.join("memory").exists(),
            "empty work keeps no compiler peak"
        );
    }
}

#[test]
fn selected_cell_work_preserves_the_normal_cold_claim_and_disk_selection() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CLAIMED: AtomicU64 = AtomicU64::new(0);
    fn refuse(
        need: aterm_capacity::Need,
        _: aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
        CLAIMED.store(need.mem, Ordering::SeqCst);
        Err(aterm_capacity::Error::Ledger(std::io::Error::other(
            "fixture refusal",
        )))
    }
    let (_dir, mut ctx) = world();
    ctx.checks = vec![Check::Census, Check::Cells];
    std::fs::write(ctx.root.join("installed"), b"yes").unwrap();
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(64 << 30),
        Some(56 << 30),
    ));
    let seams = Seams {
        stage: native,
        prepare_cells: Some(captured),
        prepare_build_dir: |_| Ok(()),
        admit_memory: refuse,
        ..crate::LIVE
    };
    let invocation = crate::RunPlan::capture(&ctx, seams).expect("complete graph and plan");
    assert!(matches!(
        invocation
            .cells
            .as_ref()
            .unwrap()
            .inputs
            .as_ref()
            .unwrap()
            .plans
            .as_ref()
            .unwrap()
            .as_slice(),
        [PreparedCell {
            plan: CellPlan::Compile(_),
            ..
        }]
    ));
    assert!(invocation.has_compiler_stage());
    assert!(invocation.compiles(StageId::ForeignCells));
    let planned_disk = crate::disk::run_planned(&invocation).unwrap().unwrap();
    let normal_disk = crate::disk::run_of(&ctx).unwrap().unwrap();
    assert_eq!(planned_disk, normal_disk);
    CLAIMED.store(0, Ordering::SeqCst);
    let mut output = Vec::new();
    let code = crate::run_with_plan(
        &ctx,
        &mut output,
        &mut std::io::sink(),
        &Show::default(),
        seams,
        Some(invocation),
        None,
    )
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert_eq!(code, crate::exit::COULD_NOT_RUN, "{output}");
    assert_eq!(CLAIMED.load(Ordering::SeqCst), 16 << 30, "{output}");
    assert!(ctx.root.join("native-ran").exists());
    assert!(
        observations(&ctx, crate::event_log::EventKind::CellFinished).is_empty(),
        "refused compiler admission starts no cell"
    );
}

#[cfg(unix)]
#[test]
fn an_empty_cell_stage_neither_borrows_a_compiler_token_nor_keeps_a_compiler_peak() {
    let (_dir, mut ctx) = world();
    ctx.scope = Scope::crate_only("other");
    ctx.checks = vec![Check::Cells];
    ctx.pool = Some(
        aterm_capacity::Jobserver::new(&ctx.root.join("jobs"), 1)
            .unwrap()
            .into(),
    );
    let memory = ctx.root.join("memory");
    ctx.memory = Some(crate::memory::Book::at(
        Some(memory.clone()),
        Some(64 << 30),
        Some(56 << 30),
    ));
    let invocation = crate::RunPlan::capture(
        &ctx,
        Seams {
            prepare_cells: Some(captured),
            ..crate::LIVE
        },
    )
    .expect("complete graph and plan");
    let spec = invocation
        .stages
        .iter()
        .find(|s| s.id == StageId::ForeignCells)
        .unwrap();
    let quiet = crate::Quiet::of(&ctx, std::slice::from_ref(spec), crate::LIVE.admit_quiet);
    let early = crate::EarlyClaims::default();
    let (report, ..) = crate::staged(
        &ctx,
        &quiet,
        spec,
        None,
        crate::LIVE,
        crate::StageAdmission::Planned {
            early: &early,
            plan: &invocation,
        },
        || {
            assert_eq!(crate::compile_width::current(), None);
            let token = ctx.pool.as_ref().unwrap().try_acquire().unwrap();
            assert!(
                token.is_some(),
                "empty cell work holds no implicit compiler token"
            );
            crate::memory::current().unwrap().book(
                4242,
                &[aterm_sysprobe::MemberUsage {
                    pid: 4242,
                    start: 1,
                    cpu_ns: 0,
                    runnable_ns: 0,
                    io_bytes: 0,
                    footprint: 1 << 30,
                    peak_footprint: 1 << 30,
                }],
            );
            let mut report = Report::new("cells");
            report.pass("metadata-only work completed");
            report
        },
    );
    assert_eq!(
        report.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
        [Outcome::Ok]
    );
    assert!(
        !memory.exists(),
        "metadata peaks cannot calibrate compiler admissions"
    );
}

#[test]
fn oracle_only_work_cannot_project_empty_compiler_admission() {
    let (_dir, mut ctx) = ground();
    ctx.scope = Scope::workspace();
    let mut prepared = captured(&ctx).unwrap();
    let inputs = prepared.inputs.as_mut().unwrap();
    inputs.cells[0].triple = "x86_64-pc-windows-msvc".into();
    inputs.plans = Some(vec![PreparedCell {
        cell: inputs.cells[0].clone(),
        plan: CellPlan::Terminal(CellOutcome::Unselected),
        resolved: Duration::ZERO,
    }]);
    assert!(
        prepared.needs_compiler(&ctx).unwrap(),
        "Win32 oracles still compile"
    );
}

#[test]
fn unknown_packages_and_unresolved_graphs_are_never_empty_plans() {
    for missing in [false, true] {
        let (_dir, mut ctx) = world();
        if missing {
            std::fs::remove_file(ctx.root.join("app/Cargo.toml")).unwrap();
        } else {
            ctx.scope = Scope::crate_only("unknown");
        }
        let mut prepared = captured(&ctx).unwrap();
        prepared.resolve();
        assert!(prepared.needs_compiler(&ctx).unwrap());
        if let Ok(inputs) = &prepared.inputs {
            assert!(inputs.plans.as_ref().unwrap().iter().all(|entry| matches!(
                &entry.plan,
                CellPlan::Terminal(CellOutcome::CouldNotRun { .. })
            )));
        }
        let mut report = Report::new("cells");
        run_prepared(&ctx, &mut report, Some(prepared));
        assert!(
            report
                .outcomes()
                .any(|(o, _)| matches!(o, Outcome::Fail(_))),
            "{}",
            report.render()
        );
    }
}

#[test]
fn prepared_cells_reject_a_different_live_context_before_projection_and_execution() {
    let (_first, mut first) = world();
    first.scope = Scope::crate_only("other");
    let (_second, second) = world();
    let mut prepared = captured(&first).unwrap();
    prepared.resolve();
    assert!(!prepared.needs_compiler(&first).unwrap());
    assert!(prepared.needs_compiler(&second).is_err());
    let mut report = Report::new("cells");
    run_prepared(&second, &mut report, Some(prepared));
    assert!(report.render().contains("another invocation"));
    assert!(!second.root.join("native-ran").exists());
    let seams = Seams {
        prepare_cells: Some(captured),
        stage: native,
        prepare_build_dir: |_| Ok(()),
        ..crate::LIVE
    };
    let invocation = crate::RunPlan::capture(&first, seams).expect("complete graph and plan");
    let mut output = Vec::new();
    let code = crate::run_with_plan(
        &second,
        &mut output,
        &mut std::io::sink(),
        &Show::default(),
        seams,
        Some(invocation),
        None,
    )
    .unwrap();
    assert_eq!(code, crate::exit::COULD_NOT_RUN);
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("another invocation")
    );
    assert!(!second.root.join("native-ran").exists());
}

/// A CHECKED CELL KEEPS WHAT ITS CALLS USED, IN ITS OWN DIRECTORY, AND A FAILED ONE NOTHING
/// ([`run_prepared`], [`plan_cell`], [`run_cell`], [`cargo_call`], [`close_passed`],
/// `crate::disk::keep_used`): a browser cell named as forge's `wasm-cpu`, so that its directory
/// is one the run keeps a record of (`crate::disk::dir_of`), and built of the fixture's member
/// `app`, is checked by the real resolver and dispatcher with a stand-in stock cargo whose lint
/// reports `app`'s metadata in the cell's directory and that its build finished: its row passes,
/// and the run keeps one record, at the cell directory's top, naming that unit in its profile and
/// `app` as the workspace's own though the lint built no test. NEGATIVE CONTROL: the same run
/// whose lint also reports an error in `app` (cargo exiting 0, its build finished) fails its row
/// (`TYPE-ERROR`) and keeps nothing. Without the close in `run_prepared`, or with the lint run
/// through [`noted`] alone, the passing run keeps nothing; with the lint noted without the
/// workspace's members, its record names none; with a close whatever the row, the control keeps
/// one.
#[cfg(unix)]
#[test]
fn a_checked_cell_keeps_what_its_calls_used_and_a_failed_one_keeps_nothing() {
    const CELL: &str = "wasm-cpu";
    const HASH: &str = "0123456789abcdef";
    let checked = |error: bool| {
        let (dir, mut ctx) = world();
        let root = ctx.root.clone();
        ctx.main_build_dir = Some(format!(
            "build.build-dir='{}'",
            root.join("cache").display()
        ));
        ctx.cached_lanes_dir = Some(root.join("lanes"));
        let leaf = cell_dir(&ctx, CELL);
        std::fs::create_dir_all(&leaf).expect("the cell's directory");
        let unit = leaf.join(format!("{TARGET}/release/deps/libapp-{HASH}.rmeta"));
        let app = format!("path+file://{}#1.0.0", root.join("app").display());
        let mut wire = vec![aterm_json::json!({
            "reason": "compiler-artifact",
            "package_id": app.as_str(),
            "target": {"kind": ["lib"], "name": "app"},
            "profile": {"test": false},
            "filenames": [unit.display().to_string()],
            "fresh": false,
        })];
        if error {
            wire.push(aterm_json::json!({
                "reason": "compiler-message",
                "package_id": app.as_str(),
                "message": {"level": "error", "rendered": "error[E0001]: a fixture's\n"},
            }));
        }
        wire.push(aterm_json::json!({"reason": "build-finished", "success": true}));
        let lines: Vec<String> = wire.iter().map(ToString::to_string).collect();
        let aside = root.join("wire.clippy.aside");
        std::fs::write(&aside, lines.join("\n") + "\n").expect("the wire");
        std::fs::rename(&aside, root.join("wire.clippy")).expect("the wire in place");
        let cargo = root.join("bin/cargo");
        crate::standin_body(&cargo, &format!("/bin/cat \"{}/wire.$1\"", root.display()));
        let prepared = PreparedRun {
            ctx: &ctx,
            inputs: Ok(Inputs {
                policy: Vec::new(),
                host: "fixture-host".into(),
                cells: vec![Cell {
                    name: CELL.into(),
                    triple: TARGET.into(),
                    package: "app".into(),
                }],
                stock: Ok(rustup::Stock {
                    toolchain: rustup::Toolchain {
                        name: "fixture-stock".into(),
                        cargo,
                        rustc: root.join("bin/rustc"),
                        clippy: true,
                    },
                    targets: vec![TARGET.into()],
                }),
                members: None,
                plans: None,
            }),
        };
        let mut report = Report::new("cells");
        run_prepared(&ctx, &mut report, Some(prepared));
        let kept = crate::disk::keep_used(&ctx);
        (dir, ctx, leaf, report, kept)
    };

    let (_dir, ctx, leaf, report, kept) = checked(false);
    let rows: Vec<Outcome> = report.outcomes().map(|(outcome, _)| outcome).collect();
    assert_eq!(rows, vec![Outcome::Ok], "{}", report.render());
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].of, crate::disk::Dir::Side(leaf.clone()), "{kept:?}");
    let record = std::fs::read(leaf.join(atpkg::build_cache::stale::USED)).expect("kept");
    let record = atpkg::build_cache::stale::Kept::decode(&record).expect("a record");
    let run = &record.runs[&crate::disk::run_key(&ctx)];
    assert_eq!(run.members, BTreeSet::from(["app".to_owned()]));
    assert_eq!(
        run.profiles,
        BTreeMap::from([(
            format!("{TARGET}/release"),
            BTreeSet::from([HASH.to_owned()])
        )])
    );

    let (_failed, _, leaf, report, kept) = checked(true);
    let text = report.render();
    let rows: Vec<Outcome> = report.outcomes().map(|(outcome, _)| outcome).collect();
    assert!(matches!(rows.as_slice(), [Outcome::Fail(_)]), "{text}");
    assert!(text.contains("TYPE-ERROR"), "{text}");
    assert!(kept.is_empty(), "{kept:?}");
    assert!(!leaf.join(atpkg::build_cache::stale::USED).exists());
}
