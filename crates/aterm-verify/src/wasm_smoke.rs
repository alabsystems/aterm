// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Web module host boundary checks ([`crate::wasm_page`], [`crate::perf::wasm::replay`]).
//!
//! STOCK EXCEPTION: Trust has no wasm32 std, so the build rides the checkout's stock pin
//! ([`crate::rustup::stock_pin`]).

use std::path::{Path, PathBuf};

use aterm_json::json;

use crate::Ctx;
use crate::exec::{self, Cmd};
use crate::ladder::Report;
use crate::perf::Status;
use crate::plan::Lane;
use crate::rustup::Toolchain;
use crate::stages::lane_jobs;
use crate::wasm_page::{self, Browser, Page, Refused};

const LABEL: &str = "wasm-smoke (both web modules' host boundary and the CPU replay, in a browser)";

/// The cells lane's `wasm-smoke` directory ([`build_dev`]).
pub(crate) fn build_dir(ctx: &Ctx) -> PathBuf {
    crate::plan::cells_dir(ctx).join("wasm-smoke")
}

/// Both web modules built in the DEV profile for wasm32 on the stock pin `toolchain`, into
/// [`build_dir`]: the directory the `.wasm` files land in, or `None` once a refusal is recorded
/// on `r`.
pub(crate) fn build_dev(
    ctx: &Ctx,
    r: &mut Report,
    toolchain: &Toolchain,
    check: &str,
    label: &str,
) -> Option<PathBuf> {
    let target_dir = build_dir(ctx);
    let cwd = ctx.scratch.join(check);
    if let Err(e) = std::fs::create_dir_all(&cwd) {
        r.cannot_run(format!("{label} — could not create {}: {e}", cwd.display()));
        return None;
    }
    let mut build = Cmd::new(&toolchain.cargo)
        .env("RUSTC", &toolchain.rustc)
        .env("RUSTUP_TOOLCHAIN", &toolchain.name)
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
    if let Some((var, value)) = toolchain.loader_env() {
        build = build.env(var, value);
    }
    let built = exec::run(&build, ctx.exec_env());
    if !built.ok {
        r.fail_child(
            &built,
            format!(
                "{check} build (aterm-wasm + aterm-gpu-web, wasm32 dev, on {})",
                toolchain.name
            ),
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
        Err(Refused::Unavailable(why) | Refused::Failed(why) | Refused::Timing(why)) => {
            return could_not_run(r, &why);
        }
    };
    let toolchain = match crate::perf::wasm::build_tools(&ctx.root, false) {
        Ok(toolchain) => toolchain,
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
    let Some(modules) = build_dev(ctx, r, &toolchain, "wasm-smoke", LABEL) else {
        return;
    };
    let site = build_dir(ctx).join("bound");
    let stamped = crate::perf::wasm::bind_modules(&modules, &site, false)
        .and_then(|()| wasm_page::lay_drivers(&ctx.root, &site))
        .and_then(|()| stamp_cpu(&ctx.root, &site));
    let version = match stamped {
        Ok(version) => version,
        Err(why) => {
            r.fail(format!("{LABEL} — {why}"));
            return;
        }
    };
    let (root, scratch, mut log) = (
        ctx.root.clone(),
        ctx.scratch.join("wasm-smoke"),
        String::new(),
    );
    let drove = exec::bounded(ctx.child_ceiling, move || {
        let run = drive(&browser, &root, &site, &scratch, &version, &mut log);
        (run, log)
    });
    match drove {
        Ok((Ok(()), log)) => {
            r.raw(log);
            r.pass(LABEL);
        }
        Ok((Err(Refused::Unavailable(why) | Refused::Timing(why)), log)) => {
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

/// Stamp the bound CPU module in `site` with the checkout `root`'s `[workspace.package]`
/// version, as `ship site` stamps the module it ships (`aterm_stamp::rewrite`): the version
/// smoke.mjs then asks the engine for (XTVERSION).
fn stamp_cpu(root: &Path, site: &Path) -> Result<String, String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("could not read {}: {e}", manifest.display()))?;
    let version = match aterm_toml::from_str::<aterm_toml::Value>(&text) {
        Ok(toml) => toml
            .get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("version"))
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_owned),
        Err(_) => None,
    }
    .ok_or_else(|| {
        format!(
            "{} names no [workspace.package] version",
            manifest.display()
        )
    })?;
    let module = site.join("cpu/aterm_wasm_bg.wasm");
    let mut bytes =
        std::fs::read(&module).map_err(|e| format!("could not read {}: {e}", module.display()))?;
    // One record per run: its version lives as long as verify.
    let stamp = aterm_stamp::Stamp {
        app_version: Box::leak(version.clone().into_boxed_str()),
        ..aterm_stamp::Stamp::UNSTAMPED
    };
    aterm_stamp::rewrite(&mut bytes, &stamp)
        .map_err(|e| format!("could not stamp {} as {version}: {e}", module.display()))?;
    std::fs::write(&module, bytes)
        .map_err(|e| format!("could not write {}: {e}", module.display()))?;
    Ok(version)
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
    version: &str,
    log: &mut String,
) -> Result<(), Refused> {
    let mut page = Page::start(browser, site, scratch)?;
    log.push_str(&format!("  browser: {}\n", page.product()));
    let ran = page.run(
        &format!("{}/smoke.mjs", wasm_page::DRIVERS),
        &json!({"cpu": "../cpu/aterm_wasm.js", "gpu": "../gpu/aterm_gpu_web.js",
                "font": format!("../{}", wasm_page::FONT), "version": version}),
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
