// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;

fn digest(bytes: &[u8]) -> String {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(bytes);
    aterm_codec::hex::encode_lower(&hash.finalize())
}

struct Example {
    root: aterm_tempfile::TempDir,
    plan: Value,
    messages: String,
    tests: Vec<TestExe>,
    index: Index,
    invocation: Vec<String>,
    run: Cmd,
    measuring: bool,
}

impl Example {
    fn new() -> Self {
        Self::with_measuring(true)
    }

    fn with_measuring(measuring: bool) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let physical = root.path().canonicalize().unwrap();
        let manifest = physical.join("Cargo.toml");
        std::os::unix::fs::symlink(&physical, physical.join("selected")).unwrap();
        fs::write(&manifest, "[package]\nname='sample'\nversion='0.1.0'\n[[test]]\nname='custom'\nharness=false\n[[bench]]\nname='paint'\nharness=false\n").unwrap();
        let mut messages = String::new();
        let mut launches = Vec::new();
        let package_id = "path+file:///sample#0.1.0";
        for (name, kind, harness, test) in [
            ("sample", "lib", true, true),
            ("custom", "test", false, true),
            ("paint", "bench", false, true),
            ("example", "example", true, true),
            ("test_bench", "bench", true, true),
            ("helper", "bin", false, false),
        ] {
            if name == "paint" && !measuring {
                continue;
            }
            let exe = physical.join(name);
            let data = b"#!/bin/sh\nexit 0\n";
            fs::write(&exe, data).unwrap();
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
            messages.push_str(&json!({"reason":"compiler-artifact","package_id":package_id,"manifest_path":manifest,"target":{"kind":[kind],"name":name},"profile":{"test":test},"executable":exe}).to_string());
            messages.push('\n');
            if test {
                launches.push(json!({"package_id":package_id,"manifest_path":manifest,"target":name,"kind":kind,"harness":harness,"tested":name!="paint","executable":exe,"program":physical.join("selected").join(name),"sha256":digest(data),"size":data.len(),"cwd":physical,"args":[],"runtime_search_paths":[],"environment":{"CARGO_PKG_NAME":"sample","RUSTC_WRAPPER":null,"TRUST_STAGE2_BIN":"/selected/compiler"}}));
            }
        }
        messages.push_str("{\"reason\":\"build-finished\",\"success\":true}\n");
        let index = Index::from_messages("tree".into(), "tool".into(), &messages).unwrap();
        let tests = crate::testrun::take_test_executables(&mut messages.clone()).unwrap();
        let scope = if measuring {
            crate::scope::Scope::workspace()
        } else {
            crate::scope::Scope::crate_only("sample")
        };
        let run = Cmd::new("unused").args(crate::stages::test_run_args(&scope));
        let mut compile = crate::stages::test_compile_args(&scope);
        if measuring {
            compile.extend(["--bench".into(), "paint".into()]);
        }
        let invocation: Vec<_> = compile.into_iter().skip(1).collect();
        let plan = json!({"schema":SCHEMA,"host":"sample-host","invocation":invocation,"target_directory":physical,"runtime_directories":[],"no_fail_fast":true,"base_environment_sha256":digest(b"base"),"available":true,"unavailable":null,"launches":launches});
        Self {
            root,
            plan,
            messages,
            tests,
            index,
            invocation,
            run,
            measuring,
        }
    }

    fn capture(&self, receipt: impl FnOnce(&mut String)) -> Result<Prepared, String> {
        let directory = aterm_tempfile::TempDir::new_in(self.root.path()).unwrap();
        let path = directory.path().join("launches.json");
        let bytes = aterm_json::to_vec(&self.plan).unwrap();
        fs::write(&path, &bytes).unwrap();
        let mut stdout = self.messages.clone();
        stdout.push_str(&json!({"schema":SCHEMA,"reason":"test-launches","available":true,"sha256":digest(&bytes),"size":bytes.len(),"path":path}).to_string());
        receipt(&mut stdout);
        let request = Request {
            directory,
            slots: aterm_tempfile::TempDir::new_in(self.root.path()).unwrap(),
            slot_count: 1,
            command: Cmd::new("unused"),
            host: "sample-host".into(),
            tools: Vec::new(),
            invocation: self.invocation.clone(),
            run_selection: RunSelection::of(&self.run),
            target_directory: self.root.path().canonicalize().unwrap(),
            measuring: self.measuring,
            compiler_environment: Vec::new(),
        };
        let mut capture = Capture::new(request, &stdout)?;
        for entry in &self.index.binaries {
            capture.digest(&entry.path).unwrap();
        }
        Prepared::capture(capture, &self.index, &self.tests)
    }

    fn launches(&mut self) -> &mut Vec<Value> {
        self.plan
            .as_object_mut()
            .unwrap()
            .get_mut("launches")
            .unwrap()
            .as_array_mut()
            .unwrap()
    }
}

#[test]
fn selected_compile_and_dispatch_must_name_the_same_packages_and_target() {
    let mut example = Example::with_measuring(false);
    let compile = Cmd::new("unused")
        .arg("--unverified")
        .args(example.invocation.iter().map(String::as_str));
    let selection = RunSelection::of(&example.run);
    assert!(selection.is_exact_selected_compile(&compile));
    let other_run = example.run.clone().args(["-p", "other"]);
    assert!(!RunSelection::of(&other_run).is_exact_selected_compile(&compile));
    assert!(!selection.is_exact_selected_compile(&compile.arg("--lib")));

    let prepared = example.capture(|_| {}).unwrap();
    assert!(prepared.run_selection == selection);
    assert!(prepared.run_selection != RunSelection::of(&other_run));
    assert_eq!(prepared.binaries(&example.run).unwrap().len(), 4);

    let original = example.plan.clone();
    example.plan.as_object_mut().unwrap().insert(
        "invocation".into(),
        json!(["test", "--workspace", "--tests", "--no-run"]),
    );
    assert!(example.capture(|_| {}).is_err());
    example.plan = original;
    let other_target = aterm_tempfile::TempDir::new_in(example.root.path()).unwrap();
    example.plan.as_object_mut().unwrap().insert(
        "target_directory".into(),
        json!(other_target.path().canonicalize().unwrap()),
    );
    assert!(example.capture(|_| {}).is_err());
}

#[test]
fn selected_compile_cannot_drop_a_measuring_target_from_its_test_population() {
    let mut example = Example::new();
    example.measuring = false;
    assert!(example.capture(|_| {}).is_err());
}

#[test]
fn exact_population_runs_tests_with_their_own_cwd_harness_and_companions() {
    let example = Example::new();
    let prepared = example.capture(|_| {}).unwrap();
    let serial = Cmd::new("unused")
        .env("RUSTC_WRAPPER", "compiler-only")
        .env("FIXTURE", "kept");
    let bins = prepared.binaries(&serial).unwrap();
    assert_eq!(bins.len(), 4);
    for bin in &bins {
        let record = &bin.record;
        assert_eq!(record.cwd, example.root.path().canonicalize().unwrap());
        assert_eq!(bin.harness, !record.exe().ends_with("custom"));
        assert_eq!(
            record.exe().parent(),
            Some(record.cwd.join("selected").as_path())
        );
        let env: BTreeMap<_, _> = record.env.iter().cloned().collect();
        assert_eq!(
            env[OsStr::new("CARGO_BIN_EXE_helper")],
            record.cwd.join("helper").as_os_str()
        );
        assert_eq!(env[OsStr::new("TRUST_STAGE2_BIN")], "/selected/compiler");
        assert_eq!(env[OsStr::new("FIXTURE")], "kept");
        assert!(!env.contains_key(OsStr::new("RUSTC_WRAPPER")));
        let ran = guard(Some(&prepared), bin, || {
            let mut child = aterm_uds::spawnfd::command(record.exe());
            let status = child
                .current_dir(&record.cwd)
                .env_clear()
                .envs(record.env.iter().cloned())
                .status()
                .unwrap();
            exec::Run {
                ok: status.success(),
                ..Default::default()
            }
        });
        assert!(ran.ok);
    }
}

#[test]
fn incomplete_or_mismatched_compiler_population_falls_back() {
    let mut sample = Example::new();
    assert!(sample.capture(|s| s.clear()).is_err());
    assert!(
        sample
            .capture(|s| {
                let dup = s.lines().last().unwrap().to_string();
                s.push('\n');
                s.push_str(&dup);
            })
            .is_err()
    );
    assert!(
        sample
            .capture(|s| {
                let changed = s.replace("\"success\":true", "\"success\":false");
                *s = changed;
            })
            .is_err()
    );
    let first = sample.launches()[0].clone();
    sample.launches()[2] = first;
    assert!(sample.capture(|_| {}).is_err());
    sample.launches().pop();
    assert!(sample.capture(|_| {}).is_err());
    let mut sample = Example::new();
    sample.launches()[0]
        .as_object_mut()
        .unwrap()
        .insert("package_id".into(), Value::from("wrong"));
    assert!(sample.capture(|_| {}).is_err());
}

#[test]
fn stale_companion_cannot_start_a_test_or_certify_a_child_that_changed_it() {
    let example = Example::new();
    let prepared = example.capture(|_| {}).unwrap();
    let bins = prepared.binaries(&Cmd::new("unused")).unwrap();
    let helper = example.root.path().join("helper");
    let result = guard(Some(&prepared), &bins[0], || {
        fs::write(&helper, b"changed").unwrap();
        exec::Run {
            ok: true,
            ..Default::default()
        }
    });
    assert!(!result.ok && result.spawn_error.is_some());
    assert!(
        guard(Some(&prepared), &bins[1], || panic!(
            "stale sibling executed"
        ))
        .spawn_error
        .is_some()
    );
    assert!(prepared.validate().is_err());
}

#[test]
fn ordinary_metadata_cannot_mint_runtime_ownership() {
    let mut sample = Example::new();
    let launch = sample.launches()[0].as_object_mut().unwrap();
    launch
        .get_mut("environment")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("CARGO_MAKEFLAGS".into(), Value::from("obsolete"));
    let captured = sample.capture(|_| {}).unwrap();
    assert!(captured.binaries(&Cmd::new("unused")).is_err());
}

#[test]
fn stale_selected_program_alias_cannot_execute() {
    let example = Example::new();
    let prepared = example.capture(|_| {}).unwrap();
    let bins = prepared.binaries(&Cmd::new("unused")).unwrap();
    let alias = example.root.path().join("selected");
    fs::remove_file(&alias).unwrap();
    fs::create_dir(&alias).unwrap();
    fs::write(alias.join("sample"), b"replacement").unwrap();
    let result = guard(Some(&prepared), &bins[0], || {
        panic!("retargeted program ran")
    });
    assert!(!result.ok && result.spawn_error.is_some());
}
