// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! `oracle-052`: crates/aterm-windows-sys-052, the windows-sys 0.52 facade
//! ring reads on aarch64 Windows (WB.1, docs/AUDIT-third-party-2026-10-15.md),
//! against the registry windows-sys 0.52.0, on both Windows triples. Its
//! reference workspace is the crate's `tests/oracle/`: no patch table, so its
//! `windows-sys = "=0.52.0"` reaches the registry, and `under-test/` names the
//! facade's own `src/lib.rs` under a second package name.
//!
//! Per triple:
//! 1. conformance: `cargo check` of the conformance crate, whose const-asserts
//!    hold the facade's types, sizes, alignments, constant and fn signature to
//!    the registry crate's;
//! 2. import: a call through each side (`probe-*`) references the same
//!    symbols, each resolved from the same DLLs: the facade's rlib bundles its
//!    raw-dylib import members, and upstream's build script names the
//!    directory of `windows.0.52.0.lib`;
//! 3. NEGATIVE CONTROLS: each `plant-*` feature swaps the facade for a copy
//!    with one planted mismatch, which conformance (an error from the
//!    conformance crate's own compiler unit) or the import comparison must
//!    catch.
//!
//! STOCK EXCEPTION as `oracle`: stock Cargo/RUSTC with the Windows std;
//! nothing is linked or run. A build or inspection that fails decides
//! nothing, and is never a caught plant.
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
    let mut clean = words(&["clean", "-q", "-p", UNDER_TEST, "-p", PACKAGE]);
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
        if l.ends_with(':') && !l.starts_with(' ') {
            member = Some(l.trim_end_matches(':').to_string());
            continue;
        }
        let p: Vec<_> = l.split_whitespace().collect();
        if p.len() == 3
            && p[1] == "T"
            && let Some(m) = &member
        {
            out.entry(p[2].to_string()).or_default().insert(m.clone());
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
            "{features} imports could not be completely inspected"
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

/// The oracle over the reference workspace in `root`, building in `target`:
/// exit 0 pass, 1 a finding, 3 could not run.
pub fn gate(root: &Path, tools: &Tools, target: &Path) -> Result<u8> {
    let mut runner = System;
    let ws = root.join(WORKSPACE);
    let mut findings = vec![];
    let measured = (|| -> Result<()> {
        for triple in TRIPLES {
            println!("== {triple} (STOCK EXCEPTION: Windows std, compile/import oracle only)");
            if !conforms(&mut runner, tools, &ws, target, triple, None)? {
                // Every probe compiles the same asserts, so nothing more builds.
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
mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[cfg(unix)]
    fn status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }
    #[cfg(windows)]
    fn status(code: i32) -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code as u32)
    }
    fn answer(code: i32, stdout: &str) -> Result<Output> {
        Ok(Output {
            status: status(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: b"controlled tool refusal".to_vec(),
        })
    }
    struct Fake(VecDeque<Result<Output>>);
    impl Runner for Fake {
        fn output(&mut self, _: &Path, _: &[String], _: &Path, _: Option<&Path>) -> Result<Output> {
            self.0
                .pop_front()
                .expect("every injected command is accounted for")
        }
    }
    fn tools() -> Tools {
        Tools {
            targo: "/controlled/targo".into(),
            trustfmt: "/controlled/trustfmt".into(),
            stock: Some(("/controlled/cargo".into(), "/controlled/rustc".into())),
            nm: "/controlled/nm".into(),
        }
    }
    fn messages() -> String {
        [("conformance", "/fixture/conformance.rlib"), ("facade", "/fixture/facade.rlib")]
            .map(|(name, file)| {
                format!(
                    r#"{{"reason":"compiler-artifact","target":{{"name":"{name}"}},"filenames":["{file}"]}}"#
                )
            })
            .join("\n")
    }

    #[test]
    fn only_the_conformance_units_error_is_a_rejection() {
        let error = |name: &str| {
            format!(
                r#"{{"reason":"compiler-message","target":{{"name":"{name}"}},"message":{{"level":"error","message":"controlled mismatch"}}}}"#
            )
        };
        for (label, result, expected) in [
            (
                "the plant is rejected",
                answer(101, &error("conformance")),
                Some(false),
            ),
            ("the facade conforms", answer(0, ""), Some(true)),
            (
                "a dependency failed",
                answer(101, &error("windows_sys")),
                None,
            ),
            ("the compiler died", answer(101, ""), None),
            (
                "cargo could not start",
                Err("controlled start failure".into()),
                None,
            ),
        ] {
            let mut runner = Fake(VecDeque::from([answer(0, ""), result]));
            let got = conforms(
                &mut runner,
                &tools(),
                Path::new("/fixture"),
                Path::new("/target"),
                TRIPLES[0],
                Some("plant-type"),
            );
            assert_eq!(got.ok(), expected, "{label}");
            assert!(runner.0.is_empty(), "{label}: the actual command path ran");
        }
    }

    #[test]
    fn unavailable_evidence_never_satisfies_an_import_plant() {
        let refs = " U __imp_IsProcessorFeaturePresent\n";
        let defs = |dll: &str| format!("{dll}:\n0000 T __imp_IsProcessorFeaturePresent\n");
        type Scenario = (&'static str, Vec<Result<Output>>, Option<&'static str>);
        let scenarios: Vec<Scenario> = vec![
            (
                "inspected",
                vec![
                    answer(0, ""),
                    answer(0, &messages()),
                    answer(0, refs),
                    answer(0, &defs("kernelbase.dll")),
                ],
                Some("kernelbase.dll"),
            ),
            ("clean failure", vec![answer(101, "")], None),
            ("build failure", vec![answer(0, ""), answer(101, "")], None),
            ("no probe rlib", vec![answer(0, ""), answer(0, "{}")], None),
            (
                "nm failed",
                vec![answer(0, ""), answer(0, &messages()), answer(1, "")],
                None,
            ),
            (
                "no reference",
                vec![
                    answer(0, ""),
                    answer(0, &messages()),
                    answer(0, ""),
                    answer(0, &defs("kernel32.dll")),
                ],
                None,
            ),
            (
                "undefined import",
                vec![
                    answer(0, ""),
                    answer(0, &messages()),
                    answer(0, refs),
                    answer(0, "kernel32.dll:\n"),
                ],
                None,
            ),
        ];
        for (label, steps, dll) in scenarios {
            let mut fake = Fake(steps.into());
            let got = imports(
                &mut fake,
                &tools(),
                Path::new("/fixture"),
                Path::new("/target"),
                TRIPLES[1],
                "probe-facade,plant-dll",
                Side::Planted,
            );
            let want = dll.map(|d| {
                Imports::from([(
                    "__imp_IsProcessorFeaturePresent".to_string(),
                    BTreeSet::from([d.to_string()]),
                )])
            });
            assert_eq!(got.ok(), want, "{label}");
            assert!(fake.0.is_empty(), "{label}: no hidden skipped command");
        }
    }

    #[test]
    fn upstream_library_is_the_build_scripts_import_library() {
        let msg = |paths: &str| {
            format!(
                r#"{{"reason":"build-script-executed","package_id":"x","linked_libs":[],"linked_paths":[{paths}]}}"#
            )
        };
        let text = [
            msg(r#""native=/reg/other/lib""#),
            msg(r#""dependency=/d","native=/reg/windows_aarch64_msvc-0.52.6/lib""#),
        ]
        .join("\n");
        let only =
            |p: &Path| p == Path::new("/reg/windows_aarch64_msvc-0.52.6/lib/windows.0.52.0.lib");
        assert_eq!(
            upstream_library(text.as_bytes(), only).unwrap(),
            Path::new("/reg/windows_aarch64_msvc-0.52.6/lib/windows.0.52.0.lib")
        );
        assert!(upstream_library(text.as_bytes(), |_| false).is_err());
        assert!(upstream_library(b"", |_| true).is_err());
    }

    #[test]
    fn every_defining_member_is_kept() {
        let text = "kernel32.dll:\n0 T __imp_A\n0 T A\n\napi-ms-win.dll:\n0 T __imp_A\n0 U B\n";
        let defs = defined_in(text);
        assert_eq!(
            defs["__imp_A"],
            BTreeSet::from(["api-ms-win.dll".to_string(), "kernel32.dll".to_string()])
        );
        assert!(!defs.contains_key("B"));
    }
}
