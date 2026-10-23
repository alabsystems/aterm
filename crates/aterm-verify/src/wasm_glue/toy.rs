// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `wasm-glue` check (`verify wasm-glue`): STOCK EXCEPTION (`wasm32-unknown-unknown`).
//! Builds the toy in a scratch workspace ([`stage`]), refuses a module carrying a host path
//! ([`host_paths`]), compares its glue with the goldens and drives it in a page
//! ([`crate::wasm_page`]); the check's second row is [`super::oracle`].

use aterm_uds::output::CommandOutput as _;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::perf::LaneRun;
use crate::wasm_page::{Browser, Page, Refused};

/// The toy's crate name, which is also its module's file stem.
pub(crate) const STEM: &str = "aterm_wasm_abi_toy";

/// The toy's directory in the tree.
pub(crate) fn toy_dir(root: &Path) -> PathBuf {
    root.join("crates/aterm-wasm-abi/tests/toy")
}

/// The committed module the native golden test binds.
pub(crate) fn fixture(root: &Path) -> PathBuf {
    toy_dir(root).join(format!("{STEM}.wasm"))
}

/// The goldens' directory.
pub(crate) fn golden_dir(root: &Path) -> PathBuf {
    toy_dir(root).join("golden")
}

/// Where a site holds the glue the driver loads, and the URL it loads it by.
pub(crate) const GLUE: &str = "glue";
const GLUE_URL: &str = "../glue/aterm_wasm_abi_toy.js";

/// The glue's text files.
pub(crate) fn is_text(name: &str) -> bool {
    !name.ends_with(".wasm")
}

/// The first line where `got` and `want` differ, for a failure message.
pub(crate) fn first_difference(got: &str, want: &str) -> String {
    for (i, (g, w)) in got.lines().zip(want.lines()).enumerate() {
        if g != w {
            return format!("line {}: got `{g}`, golden `{w}`", i + 1);
        }
    }
    format!(
        "{} lines, golden {}",
        got.lines().count(),
        want.lines().count()
    )
}

/// The check over the checkout at `root`, building in `work`.
pub(crate) fn gate(root: &Path, work: &Path, record: bool, log: &mut String) -> LaneRun {
    let browser = match crate::wasm_page::browser() {
        Ok(browser) => browser,
        Err(refused) => return refused.verdict("the toy's driver"),
    };
    let rustup = match crate::perf::wasm::build_tools(false) {
        Ok(rustup) => rustup,
        Err(unrun) => return unrun,
    };
    match run(root, &rustup, &browser, work, record, log) {
        Ok(()) => LaneRun::pass(),
        Err(refused) => refused.verdict("the toy's driver"),
    }
}

fn run(
    root: &Path,
    rustup: &Path,
    browser: &Browser,
    work: &Path,
    record: bool,
    log: &mut String,
) -> Result<(), Refused> {
    let failed = Refused::Failed;
    let module = build(root, rustup, work, log).map_err(failed)?;
    let bytes = std::fs::read(&module)
        .map_err(|e| failed(format!("could not read {}: {e}", module.display())))?;
    let found = host_paths(&bytes, root);
    if !found.is_empty() {
        return Err(failed(format!(
            "the toy's module carries host paths ({}): the staged build should leave none, \
             and the committed fixture must not (the export guard refuses home paths)",
            found.join(", ")
        )));
    }
    let site = work.join("site");
    let _ = std::fs::remove_dir_all(&site);
    let out = site.join(GLUE);
    std::fs::create_dir_all(&out)
        .map_err(|e| failed(format!("could not create {}: {e}", out.display())))?;
    let golden = golden_dir(root);
    for (name, contents) in super::glue(&bytes, STEM).map_err(failed)? {
        std::fs::write(out.join(&name), &contents)
            .map_err(|e| failed(format!("could not write {name}: {e}")))?;
        if !is_text(&name) {
            continue;
        }
        let path = golden.join(&name);
        if record {
            std::fs::create_dir_all(&golden)
                .map_err(|e| failed(format!("could not create {}: {e}", golden.display())))?;
            std::fs::write(&path, &contents)
                .map_err(|e| failed(format!("could not write {}: {e}", path.display())))?;
            continue;
        }
        let want = std::fs::read_to_string(&path).map_err(|e| {
            failed(format!(
                "no golden {} ({e}): run `targo --unverified verify glue --record-toy` and \
                 review it",
                path.display()
            ))
        })?;
        let got = String::from_utf8_lossy(&contents);
        if got != want {
            return Err(failed(format!(
                "the glue's {name} differs from its golden at {}; review the change, then \
                 `targo --unverified verify glue --record-toy`",
                first_difference(&got, &want)
            )));
        }
    }
    if record {
        let fixture = fixture(root);
        std::fs::write(&fixture, &bytes)
            .map_err(|e| failed(format!("could not write {}: {e}", fixture.display())))?;
        let _ = writeln!(log, "  recorded the goldens and {}", fixture.display());
    }

    let planted = site.join("planted");
    std::fs::create_dir_all(&planted)
        .map_err(|e| failed(format!("could not create {}: {e}", planted.display())))?;
    for entry in std::fs::read_dir(&out).map_err(|e| failed(format!("{}: {e}", out.display())))? {
        let entry = entry.map_err(|e| failed(e.to_string()))?;
        let mut contents = std::fs::read(entry.path()).map_err(|e| failed(e.to_string()))?;
        if entry.file_name().to_string_lossy().ends_with(".js") {
            let text = String::from_utf8_lossy(&contents);
            if !text.contains("return ret$ >>> 0;") {
                return Err(failed(
                    "the negative control found no u32 return to plant in".to_string(),
                ));
            }
            contents = text
                .replace("return ret$ >>> 0;", "return ret$;")
                .into_bytes();
        }
        std::fs::write(planted.join(entry.file_name()), contents)
            .map_err(|e| failed(e.to_string()))?;
    }

    let mut page = start_page(browser, root, &site, work)?;
    let _ = writeln!(log, "  browser: {}", page.product());
    let (ok, console) = drive(&mut page, GLUE_URL)?;
    for line in console.lines() {
        let _ = writeln!(log, "  {line}");
    }
    if !ok {
        return Err(failed("drive.mjs failed on the toy's glue".to_string()));
    }
    let (ok, console) = drive(&mut page, "../planted/aterm_wasm_abi_toy.js")?;
    if ok || !console.contains("FAIL web scalars u32 max is unsigned") {
        return Err(failed(format!(
            "NEGATIVE CONTROL: drive.mjs did not fail on glue that reads a u32 as signed:\n{console}"
        )));
    }
    let _ = writeln!(
        log,
        "  negative control: a u32 return read as signed fails the driver"
    );
    Ok(())
}

const STAGED: [&str; 3] = [
    "aterm-wasm-abi",
    "aterm-wasm-abi-macros",
    "aterm-macro-support",
];

fn tables(manifest: &str, prefix: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in manifest.lines() {
        if line.starts_with('[') {
            inside = line.starts_with(prefix);
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("could not create {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)
                .map_err(|e| format!("could not copy {}: {e}", src.display()))?;
        }
    }
    Ok(())
}

/// Stage the toy at the root of a scratch workspace `dir` ([`STAGED`]).
pub(super) fn stage(root: &Path, dir: &Path) -> Result<PathBuf, String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|e| format!("could not clear {}: {e}", dir.display()))?;
    }
    let toy = toy_dir(root);
    copy_tree(&toy.join("src"), &dir.join("src"))?;
    for name in STAGED {
        let from = root.join("crates").join(name);
        let to = dir.join("crates").join(name);
        copy_tree(&from.join("src"), &to.join("src"))?;
        std::fs::copy(from.join("Cargo.toml"), to.join("Cargo.toml"))
            .map_err(|e| format!("could not copy {name}'s manifest: {e}"))?;
    }
    let ws = std::fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|e| format!("could not read the workspace manifest: {e}"))?;
    let mut deps = String::from("[workspace.dependencies]\n");
    for line in tables(&ws, "[workspace.dependencies]").lines() {
        if STAGED
            .iter()
            .any(|name| line.starts_with(&format!("{name} =")))
        {
            deps.push_str(line);
            deps.push('\n');
        }
    }
    let members = STAGED
        .iter()
        .map(|n| format!("\"crates/{n}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let workspace = format!(
        "[workspace]\nmembers = [{members}]\n\n{}\n{deps}\n{}",
        tables(&ws, "[workspace.package]"),
        tables(&ws, "[workspace.lints"),
    );
    let manifest = std::fs::read_to_string(toy.join("Cargo.toml"))
        .map_err(|e| format!("could not read the toy's manifest: {e}"))?;
    if !manifest.contains("path = \"../..\"") || !manifest.contains("\n[workspace]\n") {
        return Err(
            "the toy's manifest no longer names `path = \"../..\"` and `[workspace]`, \
                    which the staging rewrites"
                .to_string(),
        );
    }
    let manifest = manifest
        .replace("path = \"../..\"", "path = \"crates/aterm-wasm-abi\"")
        .replace("\n[workspace]\n", &format!("\n{workspace}"));
    let path = dir.join("Cargo.toml");
    std::fs::write(&path, manifest)
        .map_err(|e| format!("could not write the staged manifest: {e}"))?;
    Ok(path)
}

fn host_paths(bytes: &[u8], root: &Path) -> Vec<String> {
    let mut needles = vec![
        root.display().to_string(),
        "/Users/".into(),
        "/home/".into(),
    ];
    if let Some(home) = std::env::var_os("HOME").filter(|h| h.len() > 1) {
        needles.push(home.to_string_lossy().into_owned());
    }
    let mut found = Vec::new();
    for needle in needles {
        let n = needle.as_bytes();
        let mut at = 0;
        while let Some(i) = bytes[at..].windows(n.len()).position(|w| w == n) {
            let start = at + i;
            let end = bytes[start..]
                .iter()
                .position(|b| !b.is_ascii_graphic())
                .map_or(bytes.len(), |e| start + e);
            let hit = String::from_utf8_lossy(&bytes[start..end]).into_owned();
            if !found.contains(&hit) {
                found.push(hit);
            }
            at = end.max(start + 1);
        }
    }
    found
}

fn build(root: &Path, rustup: &Path, work: &Path, log: &mut String) -> Result<PathBuf, String> {
    let manifest = stage(root, &work.join("stage"))?;
    build_staged(rustup, &manifest, &work.join("target"), "the toy", log)
}

/// Build the staged crate at `manifest` (`what`, for the log) for wasm32,
/// release, on stable, into `target`, with stable's library sources at the path
/// its standard library was built at ([`crate::rustup::Toolchain::std_source_remap`]),
/// so no host path reaches the module; the module cargo wrote.
pub(super) fn build_staged(
    rustup: &Path,
    manifest: &Path,
    target: &Path,
    what: &str,
    log: &mut String,
) -> Result<PathBuf, String> {
    // STOCK EXCEPTION: Trust lacks a wasm32 std (module doc).
    let stable = crate::rustup::Toolchain::resolve(rustup, "stable")?;
    let remap = stable.std_source_remap()?;
    let neutral = std::env::temp_dir().join(format!("aterm-wasm-glue-{}", std::process::id()));
    std::fs::create_dir_all(&neutral)
        .map_err(|e| format!("could not create {}: {e}", neutral.display()))?;
    let _ = writeln!(
        log,
        "  building {what} (wasm32, release) on {}",
        stable.name
    );
    let built = stable
        .cargo()
        .env_remove("RUSTFLAGS")
        .env("CARGO_ENCODED_RUSTFLAGS", remap)
        .env("CARGO_TARGET_DIR", target)
        .current_dir(&neutral)
        .args([
            "build",
            "--release",
            "--offline",
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
        ])
        .arg(manifest)
        .output_owned();
    let _ = std::fs::remove_dir_all(&neutral);
    match built {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            return Err(format!(
                "{what}'s wasm32 build exited {:?}:\n{}",
                o.status.code(),
                String::from_utf8_lossy(&o.stderr).trim_end()
            ));
        }
        Err(e) => return Err(format!("could not run {}: {e}", stable.cargo.display())),
    }
    Ok(target.join(format!("wasm32-unknown-unknown/release/{STEM}.wasm")))
}

/// A browser serving `site`, with the toy's driver laid in it at
/// `site/drive/drive.mjs`, its profile under `work`.
pub(super) fn start_page(
    browser: &Browser,
    root: &Path,
    site: &Path,
    work: &Path,
) -> Result<Page, Refused> {
    let into = site.join("drive");
    std::fs::create_dir_all(&into)
        .map_err(|e| Refused::Failed(format!("could not create {}: {e}", into.display())))?;
    let driver = toy_dir(root).join("drive.mjs");
    std::fs::copy(&driver, into.join("drive.mjs"))
        .map_err(|e| Refused::Failed(format!("could not copy {}: {e}", driver.display())))?;
    Page::start(browser, site, &work.join("browser"))
}

/// Run the driver on the glue at `glue` (a URL relative to the driver):
/// whether every check held, and what it printed. A driver that threw is a
/// failed run, its console kept.
pub(super) fn drive(page: &mut Page, glue: &str) -> Result<(bool, String), Refused> {
    match page.run("drive/drive.mjs", &aterm_json::json!({"glue": glue})) {
        Ok(ran) => {
            let held = ran
                .value
                .get("failures")
                .and_then(aterm_json::Value::as_u64)
                == Some(0);
            Ok((held, ran.log()))
        }
        Err(Refused::Failed(why)) => Ok((false, why)),
        Err(unavailable) => Err(unavailable),
    }
}
