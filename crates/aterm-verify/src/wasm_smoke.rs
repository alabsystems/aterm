// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Web module host boundary checks ([`crate::wasm_page`], [`crate::perf::wasm::replay`]).
//!
//! STOCK EXCEPTION: Trust has no wasm32 std, so the build rides rustup's `stable`.

use std::path::{Path, PathBuf};

use aterm_json::json;

use crate::Ctx;
use crate::exec::{self, Cmd};
use crate::ladder::Report;
use crate::perf::Status;
use crate::plan::Lane;
use crate::stages::lane_jobs;
use crate::wasm_page::{self, Browser, Page, Refused};

const LABEL: &str = "wasm-smoke (both web modules' host boundary and the CPU replay, in a browser)";

/// The cells lane's `wasm-smoke` directory ([`build_dev`]).
pub(crate) fn build_dir(ctx: &Ctx) -> PathBuf {
    crate::plan::cells_dir(ctx).join("wasm-smoke")
}

/// Both web modules built in the DEV profile for wasm32 on rustup's stable, into [`build_dir`]:
/// the directory the `.wasm` files land in, or `None` once a refusal is recorded on `r`.
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

/// Build, bind and drive both modules.
pub fn run(ctx: &Ctx, r: &mut Report) {
    let browser = match wasm_page::browser() {
        Ok(browser) => browser,
        Err(Refused::Unavailable(why) | Refused::Failed(why)) => return could_not_run(r, &why),
    };
    let rustup = match crate::perf::wasm::build_tools(false) {
        Ok(rustup) => rustup,
        Err(refused) => {
            match refused.status {
                Status::CouldNotRun { why, fix } => {
                    r.cannot_run(format!("{LABEL} — {why} (fix: {fix})"));
                }
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
    let site = build_dir(ctx).join("bound");
    if let Err(why) = crate::perf::wasm::bind_modules(&modules, &site, false)
        .and_then(|()| wasm_page::lay_drivers(&ctx.root, &site))
    {
        r.fail(format!("{LABEL} — {why}"));
        return;
    }
    let (root, scratch, mut log) = (
        ctx.root.clone(),
        ctx.scratch.join("wasm-smoke"),
        String::new(),
    );
    let drove = exec::bounded(ctx.child_ceiling, move || {
        let run = drive(&browser, &root, &site, &scratch, &mut log);
        (run, log)
    });
    match drove {
        Ok((Ok(()), log)) => {
            r.raw(log);
            r.pass(LABEL);
        }
        Ok((Err(Refused::Unavailable(why)), log)) => {
            r.raw(log);
            could_not_run(r, &why);
        }
        Ok((Err(Refused::Failed(why)), log)) => {
            r.raw(log);
            r.fail(LABEL);
            r.raw(why);
        }
        Err(why) => r.cannot_run(format!("{LABEL} — the browser {}", why.sentence())),
    }
}

fn could_not_run(r: &mut Report, why: &str) {
    r.cannot_run(format!(
        "{LABEL} — it runs in the pinned chrome-headless-shell, and none ran: {why} (fix: {})",
        wasm_page::browser_fix()
    ));
}

fn drive(
    browser: &Browser,
    root: &Path,
    site: &Path,
    scratch: &Path,
    log: &mut String,
) -> Result<(), Refused> {
    let mut page = Page::start(browser, site, scratch)?;
    log.push_str(&format!("  browser: {}\n", page.product()));
    let ran = page.run(
        &format!("{}/smoke.mjs", wasm_page::DRIVERS),
        &json!({"cpu": "../cpu/aterm_wasm.js", "gpu": "../gpu/aterm_gpu_web.js",
                "font": format!("../{}", wasm_page::FONT)}),
    )?;
    for line in &ran.console {
        log.push_str(&format!("  {line}\n"));
    }
    if ran
        .value
        .get("failures")
        .and_then(aterm_json::Value::as_u64)
        != Some(0)
    {
        return Err(Refused::Failed(
            "smoke.mjs: a host-boundary check failed (its lines above)".to_string(),
        ));
    }
    crate::perf::wasm::replay(root, site, &mut page, false, log)
}
