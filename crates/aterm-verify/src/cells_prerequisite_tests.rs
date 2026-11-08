// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selected readiness observations use the real cell resolver and the real run dispatcher.

use super::*;
use crate::checks::Check;
use crate::plan::{StageId, StageSpec};
use crate::{EnvSnapshot, Scope, Seams, Show};

const TARGET: &str = "wasm32-unknown-unknown";

fn world() -> (aterm_tempfile::TempDir, Ctx) {
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
    if let Some(cargo) = std::env::var_os("CARGO") {
        ctx.tools.targo = cargo.into();
    }
    (dir, ctx)
}

fn captured(ctx: &Ctx) -> Option<PreparedRun> {
    let installed = ctx.root.join("installed").exists();
    let clippy = !ctx.root.join("no-clippy").exists();
    Some(PreparedRun {
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
            cached: Vec::new(),
        }),
    })
}

fn notes(ctx: &Ctx) -> Vec<aterm_json::Value> {
    std::fs::read_to_string(ctx.root.join("events"))
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("verify-event: "))
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|event| event["event"].as_str() == Some("cell-prerequisite-unavailable"))
        .collect()
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

fn run(ctx: &Ctx, prepare: Option<fn(&Ctx) -> Option<PreparedRun>>) -> (i32, String) {
    let mut out = Vec::new();
    let code = crate::run_with(
        ctx,
        &mut out,
        &mut std::io::sink(),
        &Show::default(),
        Seams {
            stage: native,
            prepare_cells: prepare,
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
fn ready_inputs_need_no_early_graph_and_fake_or_absent_cells_do_not_prepare() {
    let (_dir, mut ctx) = world();
    std::fs::write(ctx.root.join("installed"), b"yes").unwrap();
    let prepared = captured(&ctx).unwrap();
    assert!(!prepared.needs_resolution());
    let inputs = prepared.inputs.unwrap();
    assert!(inputs.members.is_none());
    assert!(inputs.cached.is_empty());
    fn forbidden(_: &Ctx) -> Option<PreparedRun> {
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
    prepared.resolve_unready(&ctx);
    assert!(prepared.unavailable(&ctx).is_empty());
    assert!(
        matches!(&prepared.inputs.as_ref().unwrap().cached[0].1, CellOutcome::Failed { status, .. } if status == "SHIM-DEAD")
    );
    assert!(ctx.halted().is_none());
}

#[test]
fn a_later_halt_takes_precedence_over_the_cached_missing_tool() {
    let (_dir, mut ctx) = world();
    let mut prepared = captured(&ctx).unwrap();
    prepared.resolve_unready(&ctx);
    assert_eq!(prepared.unavailable(&ctx).len(), 1);
    ctx.halt = crate::sched::Halt::at_first_red("fixture", |_| {});
    assert!(ctx.halt.red("another-check"));
    let mut report = Report::new("cells");
    run_prepared(&ctx, &mut report, Some(prepared));
    let text = report.render();
    assert!(text.contains("NOT-STARTED"), "{text}");
    assert!(!text.contains("NO-TOOLCHAIN"), "{text}");
}

#[test]
fn preparation_admission_refusal_defers_cells_but_other_checks_still_run() {
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
                admit_memory: refuse,
                ..crate::LIVE
            },
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(code, crate::exit::COULD_NOT_RUN, "{output}");
        assert_eq!(
            ADMISSIONS.load(Ordering::SeqCst),
            if ready { 1 } else { 2 },
            "{output}"
        );
        assert!(notes(&ctx).is_empty());
        assert!(ctx.root.join("native-ran").exists());
        assert!(!output.contains("NO-TOOLCHAIN"), "{output}");
    }
}
