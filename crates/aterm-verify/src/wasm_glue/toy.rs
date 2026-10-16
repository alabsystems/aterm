// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `wasm-glue` check (`verify wasm-glue`; `verify glue --record-toy`
//! re-records): the generator on a real module.
//!
//! 1. Build the toy (`crates/aterm-wasm-abi/tests/toy`, one export per
//!    boundary type) for `wasm32-unknown-unknown` on rustup's stable.
//!    STOCK EXCEPTION: Trust has no wasm32 std. The toy and the crates it
//!    builds from are copied into one scratch workspace ([`stage`]) and
//!    built from a neutral cwd, so no repo config and no lock file in the
//!    tree is involved, and cargo hands rustc paths relative to that
//!    workspace: the module's panic locations read `crates/...`, never this
//!    checkout's path. A module that still carries a host path fails the
//!    gate ([`host_paths`]): the committed fixture ships with the source,
//!    and the export guard refuses a home path in any file.
//! 2. Bind it in both flavors and compare the text files with the goldens
//!    under `tests/toy/golden/`. `--record-toy` rewrites them, and the module
//!    fixture the native golden test (`super::tests`) binds, after review.
//! 3. Run `tests/toy/drive.mjs` under `node --expose-gc` on the fresh glue.
//! 4. NEGATIVE CONTROL: plant one wrong conversion (a `u32` return read as
//!    signed) in a copy of the glue; the driver must fail on it, by name.
//!
//! A machine without node or the stable wasm32 std is a skip with the fix.
//! Progress goes to the caller's log, never the terminal.

use aterm_uds::output::CommandOutput as _;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::command;

use super::Flavor;
use crate::perf::LaneRun;

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

/// A flavor's golden directory.
pub(crate) fn golden_dir(root: &Path, flavor: Flavor) -> PathBuf {
    toy_dir(root).join("golden").join(flavor_name(flavor))
}

pub(crate) fn flavor_name(flavor: Flavor) -> &'static str {
    match flavor {
        Flavor::Web => "web",
        Flavor::Nodejs => "nodejs",
    }
}

/// The glue's text files, which have goldens (the `_bg.wasm` is the
/// module's own bytes, and differs with the compiler).
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

/// The check over the checkout at `root`, building in `work`; its progress
/// lines are appended to `log`.
pub(crate) fn gate(root: &Path, work: &Path, record: bool, log: &mut String) -> LaneRun {
    if !command("node")
        .arg("--version")
        .output_owned()
        .is_ok_and(|o| o.status.success())
    {
        return LaneRun::skip(
            "node not found; the toy runs under node",
            "install node (brew install node)",
        );
    }
    let Some(rustup) = crate::rustup::find_rustup() else {
        return LaneRun::skip(
            format!("no rustup ({})", crate::rustup::RUSTUP_LOOKED),
            "install rustup (https://rustup.rs) and the stable wasm32 target",
        );
    };
    let has_target = command(&rustup)
        .args(["target", "list", "--installed", "--toolchain", "stable"])
        .output_owned()
        .is_ok_and(|o| {
            crate::rustup::lists_target(
                &String::from_utf8_lossy(&o.stdout),
                "wasm32-unknown-unknown",
            )
        });
    if !has_target {
        return LaneRun::skip(
            "no stable wasm32-unknown-unknown target",
            "rustup target add wasm32-unknown-unknown --toolchain stable",
        );
    }
    match run(root, &rustup, work, record, log) {
        Ok(()) => LaneRun::pass(),
        Err(e) => LaneRun::fail(e),
    }
}

fn run(
    root: &Path,
    rustup: &Path,
    work: &Path,
    record: bool,
    log: &mut String,
) -> Result<(), String> {
    let module = build(root, rustup, work, log)?;
    let bytes =
        std::fs::read(&module).map_err(|e| format!("could not read {}: {e}", module.display()))?;
    let found = host_paths(&bytes, root);
    if !found.is_empty() {
        return Err(format!(
            "the toy's module carries host paths ({}): the staged build should leave none, \
             and the committed fixture must not (the export guard refuses home paths)",
            found.join(", ")
        ));
    }
    let glue_dir = work.join("glue");
    for flavor in [Flavor::Web, Flavor::Nodejs] {
        let out = glue_dir.join(flavor_name(flavor));
        std::fs::create_dir_all(&out)
            .map_err(|e| format!("could not create {}: {e}", out.display()))?;
        let golden = golden_dir(root, flavor);
        for (name, contents) in super::glue(&bytes, STEM, flavor)? {
            std::fs::write(out.join(&name), &contents)
                .map_err(|e| format!("could not write {name}: {e}"))?;
            if !is_text(&name) {
                continue;
            }
            let path = golden.join(&name);
            if record {
                std::fs::create_dir_all(&golden)
                    .map_err(|e| format!("could not create {}: {e}", golden.display()))?;
                std::fs::write(&path, &contents)
                    .map_err(|e| format!("could not write {}: {e}", path.display()))?;
                continue;
            }
            let want = std::fs::read_to_string(&path).map_err(|e| {
                format!(
                    "no golden {} ({e}): run `targo --unverified verify glue --record-toy` and \
                     review it",
                    path.display()
                )
            })?;
            let got = String::from_utf8_lossy(&contents);
            if got != want {
                return Err(format!(
                    "the {} glue's {name} differs from its golden at {}; review the change, \
                     then `targo --unverified verify glue --record-toy`",
                    flavor_name(flavor),
                    first_difference(&got, &want)
                ));
            }
        }
    }
    if record {
        let fixture = fixture(root);
        std::fs::write(&fixture, &bytes)
            .map_err(|e| format!("could not write {}: {e}", fixture.display()))?;
        let _ = writeln!(log, "  recorded the goldens and {}", fixture.display());
    }

    let driver = toy_dir(root).join("drive.mjs");
    let (ok, stderr) = drive(&driver, &glue_dir)?;
    for line in stderr.lines() {
        let _ = writeln!(log, "  {line}");
    }
    if !ok {
        return Err("drive.mjs failed on the toy's glue".to_string());
    }

    // NEGATIVE CONTROL: the same glue with a u32 return read as signed.
    let planted = work.join("planted");
    let _ = std::fs::remove_dir_all(&planted);
    for flavor in [Flavor::Web, Flavor::Nodejs] {
        let from = glue_dir.join(flavor_name(flavor));
        let to = planted.join(flavor_name(flavor));
        std::fs::create_dir_all(&to)
            .map_err(|e| format!("could not create {}: {e}", to.display()))?;
        for entry in std::fs::read_dir(&from).map_err(|e| format!("{}: {e}", from.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let mut contents = std::fs::read(entry.path()).map_err(|e| e.to_string())?;
            if flavor == Flavor::Nodejs && entry.file_name().to_string_lossy().ends_with(".js") {
                let text = String::from_utf8_lossy(&contents);
                if !text.contains("return ret$ >>> 0;") {
                    return Err("the negative control found no u32 return to plant in".to_string());
                }
                contents = text
                    .replace("return ret$ >>> 0;", "return ret$;")
                    .into_bytes();
            }
            std::fs::write(to.join(entry.file_name()), contents).map_err(|e| e.to_string())?;
        }
    }
    let (ok, stderr) = drive(&driver, &planted)?;
    if ok || !stderr.contains("FAIL nodejs scalars u32 max is unsigned") {
        return Err(format!(
            "NEGATIVE CONTROL: drive.mjs did not fail on glue that reads a u32 as signed:\n{stderr}"
        ));
    }
    let _ = writeln!(
        log,
        "  negative control: a u32 return read as signed fails the driver"
    );
    Ok(())
}

/// The workspace crates the toy's module is built from: aterm-wasm-abi and
/// its closure. A crate that joins the closure fails the staged build, by
/// name, until it is listed here.
const STAGED: [&str; 3] = [
    "aterm-wasm-abi",
    "aterm-wasm-abi-macros",
    "aterm-macro-support",
];

/// The tables of `manifest` whose header starts with `prefix`, each from its
/// header to the next header, as written.
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

/// Copy the files under `from` into `to`, recursively.
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

/// Stage the toy at the root of a scratch workspace `dir`, with the crates
/// it builds from ([`STAGED`]: each one's manifest and `src/`) under
/// `crates/` as members. The root manifest is the toy's, with the
/// workspace's own `[workspace.package]`, lints and dependency rows for the
/// staged crates, so they build as they do in the tree.
fn stage(root: &Path, dir: &Path) -> Result<PathBuf, String> {
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

/// The absolute host paths in `bytes`, with their context: this checkout's
/// `root`, the home directory, and anything under `/Users/` or `/home/`.
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

/// Build the toy for wasm32 on stable; the module cargo wrote.
fn build(root: &Path, rustup: &Path, work: &Path, log: &mut String) -> Result<PathBuf, String> {
    // STOCK EXCEPTION: Trust lacks a wasm32 std (module doc).
    let stable = crate::rustup::Toolchain::resolve(rustup, "stable")?;
    let manifest = stage(root, &work.join("stage"))?;
    let neutral = std::env::temp_dir().join(format!("aterm-wasm-glue-{}", std::process::id()));
    std::fs::create_dir_all(&neutral)
        .map_err(|e| format!("could not create {}: {e}", neutral.display()))?;
    let _ = writeln!(
        log,
        "  building the toy (wasm32, release) on {}",
        stable.name
    );
    let built = stable
        .cargo()
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("CARGO_TARGET_DIR", work.join("target"))
        .current_dir(&neutral)
        .args([
            "build",
            "--release",
            "--offline",
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output_owned();
    let _ = std::fs::remove_dir_all(&neutral);
    match built {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            return Err(format!(
                "the toy's wasm32 build exited {:?}:\n{}",
                o.status.code(),
                String::from_utf8_lossy(&o.stderr).trim_end()
            ));
        }
        Err(e) => return Err(format!("could not run {}: {e}", stable.cargo.display())),
    }
    Ok(work.join(format!("target/wasm32-unknown-unknown/release/{STEM}.wasm")))
}

/// Run the driver on `dir`: whether it passed, and what it printed.
fn drive(driver: &Path, dir: &Path) -> Result<(bool, String), String> {
    let out = command("node")
        .arg("--expose-gc")
        .arg(driver)
        .arg(dir)
        .output_owned()
        .map_err(|e| format!("could not run node: {e}"))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}
