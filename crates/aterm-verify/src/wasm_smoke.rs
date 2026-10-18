// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify wasm-smoke`: the two web modules' host boundary under node. Both
//! modules are built in the DEV profile, whose debug-only exports (each
//! crate's `src/host.rs`) `perf/wasm/smoke.mjs` drives: the browser clocks,
//! and what crosses the boundary in each direction. They are bound as the
//! bench binds them (both by `verify glue`'s generator) and not optimised.
//! Then the CPU
//! module's boundary replay ([`crate::perf::wasm::replay`]) holds both glue
//! flavors to its recorded answers (`verify glue --record-replay` rewrites
//! them, after review).
//!
//! STOCK EXCEPTION: Trust has no wasm32 std, so the build rides rustup's
//! `stable` from a neutral cwd, in the cells lane's directory and under its job
//! cap. A machine without node or the stable wasm32 std skips with the fix,
//! never a pass.

use std::path::{Path, PathBuf};

use crate::Ctx;
use crate::exec::{self, Cmd};
use crate::ladder::Report;
use crate::perf::Status;
use crate::plan::{Lane, lane_dir};
use crate::stages::lane_jobs;

const LABEL: &str = "wasm-smoke (both web modules' host boundary and the CPU replay, under node)";

/// The node driver, beside this crate's perf data.
fn smoke_script() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("perf/wasm/smoke.mjs")
}

/// The cells lane's `wasm-smoke` directory: the dev build's target dir
/// ([`build_dev`]) and the bound modules.
pub(crate) fn build_dir(ctx: &Ctx) -> PathBuf {
    lane_dir(ctx, Lane::CellsTarget)
        .unwrap_or_else(|| ctx.state_dir().join("cells"))
        .join("wasm-smoke")
}

/// Both web modules built in the DEV profile for wasm32 on rustup's stable
/// (the STOCK EXCEPTION in the module doc), into [`build_dir`] under the
/// cells lane's job cap: the directory the `.wasm` files land in. `check`
/// (`wasm-smoke`, `browser`) names the build row; `None` once a row is
/// recorded on `r` under `label`.
pub(crate) fn build_dev(
    ctx: &Ctx,
    r: &mut Report,
    rustup: &Path,
    check: &str,
    label: &str,
) -> Option<PathBuf> {
    let stable = match crate::rustup::Toolchain::resolve(rustup, "stable") {
        Ok(tc) => tc,
        Err(why) => {
            r.cannot_run(format!("{label} — {why}"));
            return None;
        }
    };
    let target_dir = build_dir(ctx);
    let cwd = ctx.scratch.join(check);
    if let Err(e) = std::fs::create_dir_all(&cwd) {
        r.cannot_run(format!("{label} — could not create {}: {e}", cwd.display()));
        return None;
    }
    let mut build = Cmd::new(&stable.cargo)
        .env("RUSTC", &stable.rustc)
        .env("RUSTUP_TOOLCHAIN", &stable.name)
        .env("CARGO_TARGET_DIR", &target_dir)
        .args([
            "build",
            "--locked",
            "--profile",
            "dev",
            "--target",
            "wasm32-unknown-unknown",
            "-p",
            "aterm-wasm",
            "-p",
            "aterm-gpu-web",
            "--manifest-path",
        ])
        .arg(ctx.root.join("Cargo.toml"))
        .demoted();
    build.cwd = Some(cwd);
    if let Some(jobs) = lane_jobs(Lane::CellsTarget, ctx.build_jobs_ceiling()) {
        build = build.env("CARGO_BUILD_JOBS", jobs.to_string());
    }
    if let Some((var, value)) = stable.loader_env() {
        build = build.env(var, value);
    }
    let built = exec::run(&build, ctx.exec_env());
    if !built.ok {
        r.fail_child(
            &built,
            format!("{check} build (aterm-wasm + aterm-gpu-web, wasm32 dev, on stable)"),
        );
        r.raw(built.output.as_str());
        return None;
    }
    Some(target_dir.join("wasm32-unknown-unknown/debug"))
}

/// Build, bind and drive both modules; one row.
pub fn run(ctx: &Ctx, r: &mut Report) {
    let rustup = match crate::perf::wasm::lane_tools(false) {
        Ok(rustup) => rustup,
        Err(refused) => {
            match refused.status {
                Status::Skip { why, fix } => r.skip(format!("{LABEL} — {why} (fix: {fix})")),
                Status::Fail(findings) => {
                    r.fail(LABEL);
                    r.raw(findings.join("\n"));
                }
                _ => r.cannot_run(format!("{LABEL} — its tools answered no verdict")),
            }
            return;
        }
    };
    let Some(modules) = build_dev(ctx, r, &rustup, "wasm-smoke", LABEL) else {
        return;
    };
    let bound = build_dir(ctx).join("bound");
    if let Err(why) = crate::perf::wasm::bind_modules(&modules, &bound, false)
        .and_then(|()| crate::perf::wasm::bind_cpu_web(&modules, &bound))
    {
        r.fail(format!("{LABEL} — {why}"));
        return;
    }
    let smoke = Cmd::new("node").arg(smoke_script()).arg(&bound);
    let out = exec::run(&smoke, ctx.exec_env());
    r.raw(out.output.as_str());
    if !out.ok {
        r.fail_child(&out, LABEL);
        return;
    }
    let (root, mut log) = (ctx.root.clone(), String::new());
    let replayed = exec::bounded(ctx.child_ceiling, move || {
        let run = crate::perf::wasm::replay(&root, &bound, false, &mut log);
        (run, log)
    });
    match replayed {
        Ok((Ok(()), log)) => {
            r.raw(log);
            r.pass(LABEL);
        }
        Ok((Err(why), log)) => {
            r.raw(log);
            r.fail(LABEL);
            r.raw(why);
        }
        Err(why) => r.cannot_run(format!("{LABEL} — the replay {}", why.sentence())),
    }
}
