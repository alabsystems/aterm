// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Owned compiler/inspection probes.
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
    s.chars()
        .rev()
        .take(1800)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
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
        "clean",
        "-q",
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
pub fn nm(runner: &mut impl Runner, tools: &Tools, args: &[String], cwd: &Path) -> Result<String> {
    let out = required(runner.output(&tools.nm, args, cwd, None)?, "nm inspection")?;
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}
pub fn referenced(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|l| {
            let p: Vec<_> = l.split_whitespace().collect();
            (p.len() == 2 && p[0] == "U" && p[1].starts_with("__imp_")).then(|| p[1].to_string())
        })
        .collect()
}
pub fn defined(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut member = None;
    for l in text.lines() {
        if l.ends_with(':') && !l.starts_with(' ') {
            member = Some(l.trim_end_matches(':').to_string());
            continue;
        }
        let p: Vec<_> = l.split_whitespace().collect();
        if p.len() == 3
            && p[1] == "T"
            && p[2].starts_with("__imp_")
            && let Some(m) = &member
        {
            out.insert(p[2].to_string(), m.clone());
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
            "{feature} imports could not be completely inspected"
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
fn expected_imports(model: &Model, triple: &str, hand_written: usize) -> usize {
    let arch = triple.split('-').next().unwrap_or("");
    hand_written
        + model
            .variants()
            .iter()
            .filter(|it| {
                it.kind == "fn"
                    && !it
                        .arch
                        .iter()
                        .any(|a| a.contains("target_arch") && !a.contains(&format!("\"{arch}\"")))
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
        if ["target", "__pycache__", ".git"]
            .iter()
            .any(|s| e.file_name() == *s)
        {
            continue;
        }
        let kind = e.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            copy_tree(&e.path(), &to.join(e.file_name()))?
        } else if kind.is_file() {
            std::fs::copy(e.path(), to.join(e.file_name())).map_err(|e| e.to_string())?;
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
pub fn plant(krate: &Path, p: &Plant) -> Result<()> {
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
    std::fs::write(path, text.replacen(p.old, p.new, 1)).map_err(|e| e.to_string())
}
pub fn gate(root: &Path, model: &Model, tools: &Tools, target: &Path) -> Result<u8> {
    let mut runner = System;
    let workspace = root.join("win32-oracle");
    let mut findings = vec![];
    let measured = (|| -> Result<()> {
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
                println!("  ok conformance: {} declarations", model.variants().len())
            } else {
                findings.push(format!("{triple} conformance"));
            }
            let want = import_table(&mut runner, tools, &workspace, target, triple, "upstream")?;
            let expected = expected_imports(
                model,
                triple,
                hand_written_imports(&root.join("crates/aterm-win32"))?,
            );
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
                let temporary = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
                let (ws, krate) = stage(root, temporary.path())?;
                plant(&krate, p)?;
                if !checked(
                    &mut runner,
                    tools,
                    &ws,
                    target,
                    triple,
                    "aterm-win32-under-test",
                )? {
                    findings.push(format!("{triple} plant {} breaks subject itself", p.name));
                    continue;
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
    fn artifacts() -> String {
        [ ("conformance","/fixture/conformance.rlib"),("facade","/fixture/facade.rlib")].map(|(name,file)|format!(r#"{{"reason":"compiler-artifact","target":{{"name":"{name}"}},"filenames":["{file}"]}}"#)).join("\n")
    }
    #[test]
    fn a_subject_rejection_is_distinct_from_a_failed_compiler_or_dependency() {
        let error = |name: &str| {
            format!(
                r#"{{"reason":"compiler-message","target":{{"name":"{name}"}},"message":{{"level":"error","message":"controlled type mismatch"}}}}"#
            )
        };
        for (label, result, expected) in [
            (
                "conformance rejects the changed type",
                answer(1, &error("conformance")),
                Some(false),
            ),
            ("unchanged subject compiles", answer(0, ""), Some(true)),
            ("dependency failed", answer(1, &error("windows_sys")), None),
            ("compiler died without a diagnostic", answer(1, ""), None),
            (
                "compiler could not start",
                Err("controlled start failure".into()),
                None,
            ),
        ] {
            let mut runner = Fake(VecDeque::from([answer(0, ""), result]));
            let got = checked(
                &mut runner,
                &tools(),
                Path::new("/fixture"),
                Path::new("/target"),
                TRIPLES[0],
                "aterm-win32-conformance",
            );
            assert_eq!(got.ok(), expected, "{label}");
            assert!(runner.0.is_empty(), "{label}: the actual command path ran");
        }
    }
    #[test]
    fn unavailable_probes_never_satisfy_an_import_plant() {
        let want = BTreeMap::from([("__imp_PeekMessageW".into(), "user32.dll".into())]);
        let actual = |dll: &str| format!("{dll}:\n000000 T __imp_PeekMessageW\n");
        type Scenario = (&'static str, Vec<Result<Output>>, Option<bool>);
        let scenarios: Vec<Scenario> = vec![
            (
                "real DLL mismatch",
                vec![
                    answer(0, ""),
                    answer(0, &artifacts()),
                    answer(0, " U __imp_PeekMessageW\n"),
                    answer(0, &actual("kernel32.dll")),
                ],
                Some(true),
            ),
            (
                "unchanged plant",
                vec![
                    answer(0, ""),
                    answer(0, &artifacts()),
                    answer(0, " U __imp_PeekMessageW\n"),
                    answer(0, &actual("user32.dll")),
                ],
                Some(false),
            ),
            ("build failure", vec![answer(0, ""), answer(1, "")], None),
            (
                "nm nonzero",
                vec![answer(0, ""), answer(0, &artifacts()), answer(1, "")],
                None,
            ),
            (
                "nm start failure",
                vec![
                    answer(0, ""),
                    answer(0, &artifacts()),
                    Err("controlled missing nm".into()),
                ],
                None,
            ),
            (
                "missing artifact",
                vec![answer(0, ""), answer(0, "{}")],
                None,
            ),
            (
                "empty inspection",
                vec![
                    answer(0, ""),
                    answer(0, &artifacts()),
                    answer(0, ""),
                    answer(0, ""),
                ],
                None,
            ),
            ("clean failure", vec![answer(1, "")], None),
        ];
        for (name, steps, expected) in scenarios {
            let mut fake = Fake(steps.into());
            let result = caught_import(
                &want,
                import_table(
                    &mut fake,
                    &tools(),
                    Path::new("/fixture"),
                    Path::new("/target"),
                    TRIPLES[0],
                    "facade",
                ),
            );
            assert_eq!(result.ok(), expected, "{name}");
            assert!(fake.0.is_empty(), "{name}: no hidden skipped command");
        }
    }
    #[test]
    fn upstream_imports_come_from_windows_sys_and_windows() {
        let rlibs = |names: &[&str]| {
            names
                .iter()
                .map(|name| format!(r#"{{"reason":"compiler-artifact","target":{{"name":"{name}"}},"filenames":["/fixture/{name}.rlib"]}}"#))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let refs = " U __imp_PeekMessageW\n U __imp_CreateDXGIFactory1\n";
        let sys = "user32.dll:\n000000 T __imp_PeekMessageW\n";
        let windows = "dxgi.dll:\n000000 T __imp_CreateDXGIFactory1\n";
        let mut both = Fake(VecDeque::from([
            answer(0, ""),
            answer(0, &rlibs(&["conformance", "windows_sys", "windows"])),
            answer(0, refs),
            answer(0, sys),
            answer(0, windows),
        ]));
        let got = import_table(
            &mut both,
            &tools(),
            Path::new("/fixture"),
            Path::new("/target"),
            TRIPLES[0],
            "upstream",
        )
        .unwrap();
        assert_eq!(got["__imp_CreateDXGIFactory1"], "dxgi.dll");
        assert_eq!(got["__imp_PeekMessageW"], "user32.dll");
        assert!(both.0.is_empty());
        let mut missing = Fake(VecDeque::from([
            answer(0, ""),
            answer(0, &rlibs(&["conformance", "windows_sys"])),
            answer(0, refs),
            answer(0, sys),
        ]));
        assert!(
            import_table(
                &mut missing,
                &tools(),
                Path::new("/fixture"),
                Path::new("/target"),
                TRIPLES[0],
                "upstream",
            )
            .is_err()
        );
    }
    #[test]
    fn hand_written_link_lines_are_counted() {
        let dir = aterm_tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/com.rs"), "// link!( in prose\n").unwrap();
        std::fs::write(
            dir.path().join("src/dxgi.rs"),
            "link!(\"dxgi.dll\" \"system\" fn A());\n  link!(\"dxgi.dll\" \"system\" fn B());\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/d3d11.rs"),
            "link!(\"d3d11.dll\" \"system\" fn C());\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/dcomp.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/uia.rs"), "").unwrap();
        assert_eq!(hand_written_imports(dir.path()).unwrap(), 3);
        std::fs::remove_file(dir.path().join("src/com.rs")).unwrap();
        assert!(hand_written_imports(dir.path()).is_err());
    }
    #[test]
    fn plant_changes_exactly_one_real_source_occurrence() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let p = Plant {
            name: "dll",
            phase: "import",
            file: "library.rs",
            old: "user32.dll",
            new: "kernel32.dll",
        };
        std::fs::write(
            dir.path().join(p.file),
            "link!(\"user32.dll\" \"system\" fn Example());",
        )
        .unwrap();
        plant(dir.path(), &p).unwrap();
        assert!(
            read(&dir.path().join(p.file))
                .unwrap()
                .contains("kernel32.dll")
        );
        assert!(
            plant(dir.path(), &p).is_err(),
            "refuse vacuous or ambiguous plant"
        );
    }
    #[test]
    fn every_plant_finds_its_one_spot_in_aterm_win32() {
        let krate = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-win32");
        for p in PLANTS {
            let text = read(&krate.join(p.file)).unwrap();
            assert_eq!(
                text.matches(p.old).count(),
                1,
                "plant {} in {}",
                p.name,
                p.file
            );
        }
    }
}
