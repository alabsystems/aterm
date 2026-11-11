// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Owned compiler/inspection probes.
//!
//! STOCK EXCEPTION: [`cargo`] runs the absolute stock Cargo and rustc the driver resolved,
//! because Trust ships no Windows std. It compiles and reads import tables: no Windows
//! program runs.
use crate::model::Model;
use crate::plants::PLANTS;
use crate::{Result, TRIPLES, Tools, read, run_status};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Output;

pub struct Plant {
    pub name: &'static str,
    pub phase: &'static str,
    pub file: &'static str,
    pub old: &'static str,
    pub new: &'static str,
}
pub trait Runner {
    fn output(
        &mut self,
        program: &Path,
        args: &[String],
        cwd: &Path,
        rustc: Option<&Path>,
    ) -> Result<Output>;
}
pub struct System;
impl Runner for System {
    fn output(
        &mut self,
        p: &Path,
        args: &[String],
        cwd: &Path,
        rustc: Option<&Path>,
    ) -> Result<Output> {
        run_status(p, args, cwd, rustc)
    }
}
pub fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}
pub fn tail(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let start = s.char_indices().rev().nth(1799).map_or(0, |(idx, _)| idx);
    s[start..].to_string()
}
pub fn required(out: Output, label: &str) -> Result<Output> {
    if !out.status.success() {
        return Err(format!(
            "{label} failed: {}{}",
            tail(&out.stdout),
            tail(&out.stderr)
        ));
    }
    Ok(out)
}
/// The flags every oracle Cargo command carries, the clean before a build
/// included: the reference workspace answers from its committed `Cargo.lock`
/// and the local cache, so lock drift is could-not-run.
pub const FROZEN: [&str; 2] = ["--locked", "--offline"];
pub fn cargo(
    runner: &mut impl Runner,
    tools: &Tools,
    workspace: &Path,
    target: &Path,
    args: &[&str],
) -> Result<Output> {
    let (cargo, rustc) = tools
        .stock
        .as_ref()
        .ok_or("oracle requires absolute stock Cargo/RUSTC (STOCK EXCEPTION for Windows std)")?;
    let target = target.to_str().ok_or("non-UTF8 target directory")?;
    let mut clean = words(&[
        // Not `-q`: a quiet clean that cargo refuses exits 101 saying nothing.
        "clean",
        "-p",
        "aterm-win32-under-test",
        "-p",
        "aterm-win32-conformance",
    ]);
    clean.extend(words(&FROZEN));
    clean.extend(words(&["--target-dir", target]));
    if let Some(t) = args
        .iter()
        .find(|a| a.starts_with("x86_64-") || a.starts_with("aarch64-"))
    {
        clean.extend(words(&["--target", t]));
    }
    required(
        runner.output(cargo, &clean, workspace, Some(rustc))?,
        "clean oracle units",
    )?;
    let mut args = words(args);
    args.extend(words(&FROZEN));
    args.extend(words(&["--target-dir", target]));
    runner.output(cargo, &args, workspace, Some(rustc))
}
pub fn artifacts(messages: &[u8]) -> Result<BTreeMap<String, PathBuf>> {
    let mut out = BTreeMap::new();
    for line in String::from_utf8_lossy(messages).lines() {
        if !line.contains("compiler-artifact") {
            continue;
        }
        let v: Value = aterm_json::from_str(line).map_err(|e| e.to_string())?;
        if v.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let name = v
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(Value::as_str)
            .ok_or("artifact lacks target name")?;
        for p in v
            .get("filenames")
            .and_then(Value::as_array)
            .ok_or("artifact lacks filenames")?
        {
            let p = p.as_str().ok_or("artifact filename isn't a string")?;
            if p.ends_with(".rlib") {
                out.insert(name.into(), p.into());
            }
        }
    }
    Ok(out)
}
/// What an import inspection that fell short adds to its refusal: off macOS, where the nm that
/// read it is not LLVM's, that nm and where LLVM's comes from, since GNU nm reads no Windows
/// import library ([`crate::Tools`]' nm: `$NM`, `llvm-nm` on `PATH`, the stock pin's
/// llvm-tools, then `nm`); otherwise nothing.
pub fn nm_hint(tools: &Tools) -> String {
    let llvm = tools
        .nm
        .file_name()
        .is_some_and(|n| n.to_string_lossy().contains("llvm-nm"));
    if cfg!(target_os = "macos") || llvm {
        String::new()
    } else {
        format!(
            " (read with {}, not llvm-nm: `targo --unverified verify provision rustup` gives the \
             stock pin the llvm-tools whose llvm-nm reads Windows import libraries)",
            tools.nm.display()
        )
    }
}
pub fn nm(runner: &mut impl Runner, tools: &Tools, args: &[String], cwd: &Path) -> Result<String> {
    let out = required(runner.output(&tools.nm, args, cwd, None)?, "nm inspection")?;
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}
pub fn referenced(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|l| {
            let mut p = l.split_whitespace();
            let (Some("U"), Some(sym), None) = (p.next(), p.next(), p.next()) else {
                return None;
            };
            sym.starts_with("__imp_").then(|| sym.to_string())
        })
        .collect()
}
pub fn defined(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut member = None;
    for l in text.lines() {
        if let Some(m) = l.strip_suffix(':').filter(|m| !m.starts_with(' ')) {
            member = Some(m);
            continue;
        }
        let mut p = l.split_whitespace();
        if let (Some(_), Some("T"), Some(sym), None) = (p.next(), p.next(), p.next(), p.next())
            && sym.starts_with("__imp_")
            && let Some(m) = member
        {
            out.insert(sym.to_string(), m.to_string());
        }
    }
    out
}
pub fn import_table(
    runner: &mut impl Runner,
    tools: &Tools,
    workspace: &Path,
    target: &Path,
    triple: &str,
    side: &str,
) -> Result<BTreeMap<String, String>> {
    let feature = format!("probe-{side}");
    let out = required(
        cargo(
            runner,
            tools,
            workspace,
            target,
            &[
                "build",
                "-q",
                "-p",
                "aterm-win32-conformance",
                "--target",
                triple,
                "--features",
                &feature,
                "--message-format=json",
            ],
        )?,
        &format!("{feature} build"),
    )?;
    let libs = artifacts(&out.stdout)?;
    let conformance = libs
        .get("conformance")
        .ok_or("probe did not report conformance rlib")?;
    let subjects: &[&str] = if side == "facade" {
        &["facade"]
    } else {
        &["windows_sys", "windows"]
    };
    let refs = referenced(&nm(
        runner,
        tools,
        &["-u".into(), conformance.to_string_lossy().into_owned()],
        workspace,
    )?);
    let mut defs = BTreeMap::new();
    for subject in subjects {
        let library = libs
            .get(*subject)
            .ok_or("probe did not report subject rlib")?;
        defs.extend(defined(&nm(
            runner,
            tools,
            &[library.to_string_lossy().into_owned()],
            workspace,
        )?));
    }
    if refs.is_empty() || refs.iter().any(|s| !defs.contains_key(s)) {
        return Err(format!(
            "{feature} imports could not be completely inspected{}",
            crate::oracle::nm_hint(tools)
        ));
    }
    Ok(refs
        .into_iter()
        .map(|s| {
            let dll = defs[&s].clone();
            (s, dll)
        })
        .collect())
}
/// Only two successfully inspected tables can differ.
pub fn caught_import(
    want: &BTreeMap<String, String>,
    got: Result<BTreeMap<String, String>>,
) -> Result<bool> {
    Ok(got? != *want)
}
/// The imports `triple`'s probes must name: every generated function the
/// model keeps for its architecture, plus each `link!` of aterm-win32's
/// hand-written files (`hand_written`, counted by [`hand_written_imports`]).
fn expected_imports(variants: &[&crate::source::Item], triple: &str, hand_written: usize) -> usize {
    let arch = triple.split('-').next().unwrap_or("");
    let arch_needle = format!("\"{arch}\"");
    hand_written
        + variants
            .iter()
            .filter(|it| {
                it.kind == "fn"
                    && !it
                        .arch
                        .iter()
                        .any(|a| a.contains("target_arch") && !a.contains(&arch_needle))
            })
            .map(|it| &it.name)
            .collect::<BTreeSet<_>>()
            .len()
}
/// The `link!` imports aterm-win32's hand-written files declare (the DXGI
/// slice's `CreateDXGIFactory1`, Direct3D 11's `D3D11CreateDevice` and
/// DirectComposition's `DCompositionCreateDevice`; UI Automation's are
/// generated): none of them is in the generated model.
pub fn hand_written_imports(krate: &Path) -> Result<usize> {
    let mut count = 0;
    for file in crate::model::HAND_WRITTEN {
        count += read(&krate.join("src").join(file))?
            .lines()
            .filter(|l| l.trim_start().starts_with("link!("))
            .count();
    }
    Ok(count)
}
fn checked(
    runner: &mut impl Runner,
    tools: &Tools,
    workspace: &Path,
    target: &Path,
    triple: &str,
    package: &str,
) -> Result<bool> {
    let out = cargo(
        runner,
        tools,
        workspace,
        target,
        &[
            "check",
            "-q",
            "-p",
            package,
            "--target",
            triple,
            "--message-format=json",
        ],
    )?;
    if out.status.success() {
        return Ok(true);
    }
    let target_name = if package == "aterm-win32-conformance" {
        "conformance"
    } else {
        "facade"
    };
    if !rejected_by(&out.stdout, target_name) {
        return Err(format!(
            "{package} did not produce a subject compiler diagnostic: {}{}",
            tail(&out.stdout),
            tail(&out.stderr)
        ));
    }
    Ok(false)
}
/// Whether cargo's JSON `messages` hold an error from library target
/// `target_name`'s own compiler unit: a subject's rejection, as against a
/// dependency or a compiler that failed.
pub fn rejected_by(messages: &[u8], target_name: &str) -> bool {
    String::from_utf8_lossy(messages).lines().any(|line| {
        let Ok(v) = aterm_json::from_str::<Value>(line) else {
            return false;
        };
        v.get("reason").and_then(Value::as_str) == Some("compiler-message")
            && v.get("target")
                .and_then(|t| t.get("name"))
                .and_then(Value::as_str)
                == Some(target_name)
            && v.get("message")
                .and_then(|m| m.get("level"))
                .and_then(Value::as_str)
                == Some("error")
    })
}
pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name();
        if name == "target" || name == "__pycache__" || name == ".git" {
            continue;
        }
        let kind = e.file_type().map_err(|e| e.to_string())?;
        let src = e.path();
        if kind.is_dir() {
            copy_tree(&src, &to.join(&name))?
        } else if kind.is_file() {
            let (mut reader, meta) =
                aterm_uds::regular_file::open(&src).map_err(|e| e.to_string())?;
            let dst = to.join(&name);
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
            }
            let mut writer = opts.open(&dst).map_err(|e| e.to_string())?;
            std::io::copy(&mut reader, &mut writer).map_err(|e| e.to_string())?;
            writer
                .set_permissions(meta.permissions())
                .map_err(|e| e.to_string())?;
        } else {
            return Err(format!(
                "refusing nonregular oracle input {}",
                e.path().display()
            ));
        }
    }
    Ok(())
}
pub fn stage(root: &Path, tmp: &Path) -> Result<(PathBuf, PathBuf)> {
    let workspace = tmp.join("win32-oracle");
    let krate = tmp.join("crates/aterm-win32");
    copy_tree(&root.join("win32-oracle"), &workspace)?;
    copy_tree(&root.join("crates/aterm-win32/src"), &krate.join("src"))?;
    Ok((workspace, krate))
}
pub fn plant(krate: &Path, p: &Plant) -> Result<(PathBuf, String)> {
    let path = krate.join(p.file);
    let text = read(&path)?;
    let count = text.matches(p.old).count();
    if count != 1 {
        return Err(format!(
            "plant {} matched {count} times in {}",
            p.name,
            path.display()
        ));
    }
    crate::write_fresh(&path, text.replacen(p.old, p.new, 1)).map_err(|e| e.to_string())?;
    Ok((path, text))
}
pub fn gate(root: &Path, model: &Model, tools: &Tools, target: &Path) -> Result<u8> {
    let mut runner = System;
    let workspace = root.join("win32-oracle");
    let mut findings = vec![];
    let measured = (|| -> Result<()> {
        let variants = model.variants();
        let hand_written = hand_written_imports(&root.join("crates/aterm-win32"))?;
        let temporary = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
        let (ws, krate) = stage(root, temporary.path())?;
        for triple in TRIPLES {
            println!("== {triple} (STOCK EXCEPTION: Windows std, compile/import oracle only)");
            if checked(
                &mut runner,
                tools,
                &workspace,
                target,
                triple,
                "aterm-win32-conformance",
            )? {
                println!("  ok conformance: {} declarations", variants.len())
            } else {
                findings.push(format!("{triple} conformance"));
            }
            let want = import_table(&mut runner, tools, &workspace, target, triple, "upstream")?;
            let expected = expected_imports(&variants, triple, hand_written);
            if want.len() != expected {
                return Err(format!(
                    "upstream gave {} of {expected} imports",
                    want.len()
                ));
            }
            let got = import_table(&mut runner, tools, &workspace, target, triple, "facade")?;
            if got != want {
                findings.push(format!("{triple} imports"));
            } else {
                println!("  ok imports: {} symbols, same DLLs", want.len())
            }
            for p in PLANTS {
                let (planted_path, original) = plant(&krate, p)?;
                let check_res = (|| -> Result<()> {
                    if !checked(
                        &mut runner,
                        tools,
                        &ws,
                        target,
                        triple,
                        "aterm-win32-under-test",
                    )? {
                        findings.push(format!("{triple} plant {} breaks subject itself", p.name));
                        return Ok(());
                    }
                    let conforms = checked(
                        &mut runner,
                        tools,
                        &ws,
                        target,
                        triple,
                        "aterm-win32-conformance",
                    )?;
                    let caught = if p.phase == "conformance" {
                        !conforms
                    } else {
                        conforms
                            && caught_import(
                                &want,
                                import_table(&mut runner, tools, &ws, target, triple, "facade"),
                            )?
                    };
                    if caught {
                        println!("  ok plant {} caught by {}", p.name, p.phase)
                    } else {
                        findings.push(format!("{triple} plant {} not caught", p.name));
                    }
                    Ok(())
                })();
                let restore_res =
                    crate::write_fresh(&planted_path, original).map_err(|e| e.to_string());
                check_res?;
                restore_res?;
            }
        }
        Ok(())
    })();
    if let Err(error) = measured {
        eprintln!("win32 oracle could not run: {error}");
        if findings.is_empty() {
            return Ok(3);
        }
    }
    if findings.is_empty() {
        println!("win32 oracle: PASS");
        Ok(0)
    } else {
        for f in findings {
            eprintln!("FAIL {f}")
        }
        Ok(1)
    }
}

#[cfg(test)]
#[path = "oracle_tests.rs"]
mod tests;
