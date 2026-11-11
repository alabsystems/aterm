// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! `oracle-052`: crates/aterm-windows-sys-052, the windows-sys 0.52 facade
//! ring reads on aarch64 Windows, against the registry windows-sys 0.52.0, on
//! both Windows triples.
//!
//! STOCK EXCEPTION as `oracle`: stock Cargo/RUSTC with the Windows std; nothing is linked or
//! run.
use crate::oracle::{
    FROZEN, Runner, System, artifacts, nm, referenced, rejected_by, required, tail, words,
};
use crate::{Result, TRIPLES, Tools};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Output;

/// The reference workspace, from the checkout root.
pub const WORKSPACE: &str = "crates/aterm-windows-sys-052/tests/oracle";
const PACKAGE: &str = "windows-sys-052-conformance";
/// The facade under its second package name.
const UNDER_TEST: &str = "windows-sys-052-under-test";
/// The plants const-asserts or a coercion must reject.
const CONFORMANCE_PLANTS: [&str; 6] = ["type", "size", "const", "abi", "arg", "ret"];
/// The plants only the compiled import shows.
const IMPORT_PLANTS: [&str; 2] = ["link-name", "dll"];
/// The import library upstream's `windows_*_msvc` build script links.
const UPSTREAM_LIB: &str = "windows.0.52.0.lib";

/// Which library declares the probe's import.
#[derive(Clone, Copy)]
enum Side {
    /// windows.0.52.0.lib, from upstream's build script.
    Upstream,
    /// The facade's rlib.
    Facade,
    /// The probe's own rlib, which holds the planted copy.
    Planted,
}

/// Each referenced import, with the archive members (DLLs) that define it.
type Imports = BTreeMap<String, BTreeSet<String>>;

/// Stock cargo on `workspace` with `args`, after cleaning the two path units
/// for `triple`: cargo keys a path unit by its place in the workspace, so a
/// target directory another checkout shares could otherwise answer with that
/// checkout's facade. Both commands are [`FROZEN`], so a stale `Cargo.lock`
/// stops the run instead of being rewritten.
fn cargo(
    runner: &mut impl Runner,
    tools: &Tools,
    workspace: &Path,
    target: &Path,
    triple: &str,
    args: &[&str],
) -> Result<Output> {
    let (cargo, rustc) = tools.stock.as_ref().ok_or(
        "oracle-052 requires absolute stock Cargo/RUSTC (STOCK EXCEPTION for Windows std)",
    )?;
    let target = target.to_str().ok_or("non-UTF8 target directory")?;
    // Not `-q`: a quiet clean that cargo refuses exits 101 saying nothing.
    let mut clean = words(&["clean", "-p", UNDER_TEST, "-p", PACKAGE]);
    clean.extend(words(&FROZEN));
    clean.extend(words(&["--target", triple, "--target-dir", target]));
    required(
        runner.output(cargo, &clean, workspace, Some(rustc))?,
        "clean oracle units",
    )?;
    let mut args = words(args);
    args.extend(words(&["--target", triple]));
    args.extend(words(&FROZEN));
    args.extend(words(&["--target-dir", target]));
    runner.output(cargo, &args, workspace, Some(rustc))
}

/// Whether the conformance crate checks under `features`: `false` only on an
/// error from its own compiler unit.
fn conforms(
    runner: &mut impl Runner,
    tools: &Tools,
    workspace: &Path,
    target: &Path,
    triple: &str,
    features: Option<&str>,
) -> Result<bool> {
    let mut args = vec!["check", "-q", "-p", PACKAGE, "--message-format=json"];
    if let Some(features) = features {
        args.extend(["--features", features]);
    }
    let out = cargo(runner, tools, workspace, target, triple, &args)?;
    if out.status.success() {
        return Ok(true);
    }
    if !rejected_by(&out.stdout, "conformance") {
        return Err(format!(
            "{PACKAGE} {} did not produce a conformance compiler diagnostic: {}{}",
            features.unwrap_or("(no features)"),
            tail(&out.stdout),
            tail(&out.stderr)
        ));
    }
    Ok(false)
}

/// The import library upstream's build script names: the first `native=`
/// directory of a `build-script-executed` message that holds
/// [`UPSTREAM_LIB`] (`is_file`, injected).
fn upstream_library(messages: &[u8], is_file: impl Fn(&Path) -> bool) -> Result<PathBuf> {
    for line in String::from_utf8_lossy(messages).lines() {
        if !line.contains("build-script-executed") {
            continue;
        }
        let v: Value = aterm_json::from_str(line).map_err(|e| e.to_string())?;
        if v.get("reason").and_then(Value::as_str) != Some("build-script-executed") {
            continue;
        }
        let paths = v
            .get("linked_paths")
            .and_then(Value::as_array)
            .ok_or("build-script-executed lacks linked_paths")?;
        for p in paths.iter().filter_map(Value::as_str) {
            if let Some(dir) = p.strip_prefix("native=") {
                let lib = Path::new(dir).join(UPSTREAM_LIB);
                if is_file(&lib) {
                    return Ok(lib);
                }
            }
        }
    }
    Err(format!(
        "no build script named a directory holding {UPSTREAM_LIB}"
    ))
}

/// Every symbol `nm` lists as defined (`T`), with each archive member that
/// defines it.
fn defined_in(text: &str) -> Imports {
    let mut out = Imports::new();
    let mut member = None;
    for l in text.lines() {
        if let Some(m) = l.strip_suffix(':').filter(|m| !m.starts_with(' ')) {
            member = Some(m);
            continue;
        }
        let mut p = l.split_whitespace();
        if let (Some(_), Some("T"), Some(sym), None) = (p.next(), p.next(), p.next(), p.next())
            && let Some(m) = member
        {
            out.entry(sym.to_string())
                .or_default()
                .insert(m.to_string());
        }
    }
    out
}

/// Build the probe with `features` and read its import: each `__imp_`
/// symbol it references, with the members of `side`'s library that define
/// it. A build, an inspection or an import nothing defines is an error.
fn imports(
    runner: &mut impl Runner,
    tools: &Tools,
    workspace: &Path,
    target: &Path,
    triple: &str,
    features: &str,
    side: Side,
) -> Result<Imports> {
    let out = required(
        cargo(
            runner,
            tools,
            workspace,
            target,
            triple,
            &[
                "build",
                "-q",
                "-p",
                PACKAGE,
                "--features",
                features,
                "--message-format=json",
            ],
        )?,
        &format!("{features} build"),
    )?;
    let libs = artifacts(&out.stdout)?;
    let probe = libs
        .get("conformance")
        .ok_or("probe did not report the conformance rlib")?;
    let library = match side {
        Side::Upstream => upstream_library(&out.stdout, Path::is_file)?,
        Side::Facade => libs
            .get("facade")
            .ok_or("probe did not report the facade rlib")?
            .clone(),
        Side::Planted => probe.clone(),
    };
    let refs = referenced(&nm(
        runner,
        tools,
        &["-u".into(), probe.to_string_lossy().into_owned()],
        workspace,
    )?);
    let defs = defined_in(&nm(
        runner,
        tools,
        &[library.to_string_lossy().into_owned()],
        workspace,
    )?);
    if refs.is_empty() || refs.iter().any(|s| !defs.contains_key(s)) {
        return Err(format!(
            "{features} imports could not be completely inspected{}",
            crate::oracle::nm_hint(tools)
        ));
    }
    Ok(refs
        .into_iter()
        .map(|s| {
            let dlls = defs[&s].clone();
            (s, dlls)
        })
        .collect())
}

/// `sym from dll, …`, for a verdict line.
fn show(imports: &Imports) -> String {
    imports
        .iter()
        .map(|(sym, dlls)| {
            let dlls: Vec<&str> = dlls.iter().map(String::as_str).collect();
            format!("{sym} from {}", dlls.join(" "))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The oracle over the reference workspace in `root`, building in `target`.
pub fn gate(root: &Path, tools: &Tools, target: &Path) -> Result<u8> {
    let mut runner = System;
    let ws = root.join(WORKSPACE);
    let mut findings = vec![];
    let measured = (|| -> Result<()> {
        for triple in TRIPLES {
            println!("== {triple} (STOCK EXCEPTION: Windows std, compile/import oracle only)");
            if !conforms(&mut runner, tools, &ws, target, triple, None)? {
                findings.push(format!(
                    "{triple} conformance: the facade differs from windows-sys 0.52.0 (its \
                     import and plants were not judged)"
                ));
                continue;
            }
            println!("  ok conformance: types, sizes, alignments, constant, fn signature");
            let want = imports(
                &mut runner,
                tools,
                &ws,
                target,
                triple,
                "probe-upstream",
                Side::Upstream,
            )?;
            let got = imports(
                &mut runner,
                tools,
                &ws,
                target,
                triple,
                "probe-facade",
                Side::Facade,
            )?;
            if got == want {
                println!("  ok import: {}", show(&want));
            } else {
                findings.push(format!(
                    "{triple} import: facade {}, upstream {}",
                    show(&got),
                    show(&want)
                ));
            }
            for plant in CONFORMANCE_PLANTS {
                let feature = format!("plant-{plant}");
                if conforms(&mut runner, tools, &ws, target, triple, Some(&feature))? {
                    findings.push(format!(
                        "{triple} {feature} compiled: a conformance check passes vacuously"
                    ));
                } else {
                    println!("  ok {feature} caught by conformance");
                }
            }
            for plant in IMPORT_PLANTS {
                let features = format!("probe-facade,plant-{plant}");
                let planted = imports(
                    &mut runner,
                    tools,
                    &ws,
                    target,
                    triple,
                    &features,
                    Side::Planted,
                )?;
                if planted == want {
                    findings.push(format!(
                        "{triple} plant-{plant} gave upstream's import ({}): the import check \
                         passes vacuously",
                        show(&planted)
                    ));
                } else {
                    println!("  ok plant-{plant} caught by import ({})", show(&planted));
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = measured {
        eprintln!("windows-sys 0.52 oracle could not run: {error}");
        if findings.is_empty() {
            return Ok(3);
        }
    }
    if findings.is_empty() {
        println!("windows-sys 0.52 oracle: PASS");
        Ok(0)
    } else {
        for f in findings {
            eprintln!("FAIL {f}");
        }
        Ok(1)
    }
}

#[cfg(test)]
#[path = "facade052_tests.rs"]
mod tests;
