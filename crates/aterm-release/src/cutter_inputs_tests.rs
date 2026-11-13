// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture(root: &Path) -> Vec<u8> {
    let package = root.join("crates/cutter");
    let unit = root.join("units/cutter");
    std::fs::create_dir_all(package.join("assets")).unwrap();
    std::fs::create_dir_all(unit.join("out")).unwrap();
    std::fs::create_dir_all(unit.join("fingerprint")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.package]\nversion='1.0.0'\n",
    )
    .unwrap();
    std::fs::write(root.join("Cargo.lock"), "version=4\n[[package]]\nname='cutter'\nversion='0.0.0'\ndependencies=['dep 1.0.0']\n[[package]]\nname='app'\nversion='1.0.0'\n").unwrap();
    std::fs::write(
        package.join("Cargo.toml"),
        "[package]\nname='cutter'\nversion='0.0.0'\n",
    )
    .unwrap();
    std::fs::write(package.join("source.rs"), "fn main() {}\n").unwrap();
    std::fs::write(package.join("assets/input"), "build script reads this\n").unwrap();
    std::fs::write(
        unit.join("out/cutter.d"),
        format!("output: {}\n", package.join("source.rs").display()),
    )
    .unwrap();
    std::fs::write(
        unit.join("fingerprint/bin.json"),
        r#"{"rustflags":["-Zthreads=4"],"local":[]}"#,
    )
    .unwrap();
    let generated = root.join("units/run");
    std::fs::create_dir_all(generated.join("out")).unwrap();
    std::fs::create_dir_all(generated.join("run")).unwrap();
    std::fs::write(
        generated.join("run/stdout"),
        "cargo::rerun-if-changed=assets\n",
    )
    .unwrap();
    std::fs::create_dir_all(generated.join("fingerprint")).unwrap();
    std::fs::write(
        generated.join("fingerprint/run.json"),
        r#"{"local":[{"RerunIfChanged":{"paths":["assets"]}}]}"#,
    )
    .unwrap();
    let artifact = aterm_json::json!({
        "reason":"compiler-artifact", "fresh":true, "package_id":"path+file:///fixture#cutter@0.0.0",
        "manifest_path":package.join("Cargo.toml"), "filenames":[unit.join("out/cutter")],
    });
    let script = aterm_json::json!({
        "reason":"build-script-executed", "package_id":"path+file:///fixture#cutter@0.0.0",
        "out_dir": generated.join("out"),
    });
    format!("{artifact}\n{script}\n").into_bytes()
}

#[test]
fn runtime_version_bump_keeps_inputs_but_source_lock_flags_and_script_reads_do_not() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = fixture(root);
    let seal = "a".repeat(64);
    let first = capture(root, &output, &seal).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.package]\nversion='2.0.0'\n",
    )
    .unwrap();
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
    std::fs::write(
        root.join("Cargo.lock"),
        lock.replace("name='app'\nversion='1.0.0'", "name='app'\nversion='2.0.0'"),
    )
    .unwrap();
    assert_eq!(capture(root, &output, &seal).unwrap().inputs, first.inputs);
    for path in [
        "crates/cutter/source.rs",
        "crates/cutter/assets/input",
        "Cargo.lock",
        "units/cutter/fingerprint/bin.json",
        "units/run/run/stdout",
    ] {
        let path = root.join(path);
        let original = std::fs::read(&path).unwrap();
        let changed = if path.ends_with("Cargo.lock") {
            String::from_utf8(original.clone())
                .unwrap()
                .replace("dep 1.0.0", "dep 2.0.0")
        } else if path.extension().is_some_and(|ext| ext == "json") {
            r#"{"rustflags":["-Zthreads=8"],"local":[]}"#.into()
        } else {
            "changed input, identical output binary\n".into()
        };
        std::fs::write(&path, changed).unwrap();
        assert_ne!(
            capture(root, &output, &seal).unwrap().inputs,
            first.inputs,
            "{}",
            path.display()
        );
        std::fs::write(&path, original).unwrap();
    }
    assert_ne!(
        capture(root, &output, &"b".repeat(64)).unwrap().inputs,
        first.inputs
    );
    let compiled = String::from_utf8(output.clone())
        .unwrap()
        .replace("\"fresh\":true", "\"fresh\":false");
    let compiled = capture(root, compiled.as_bytes(), &seal).unwrap();
    assert_eq!(compiled.inputs, first.inputs);
    assert!(!compiled.all_fresh);
    std::fs::write(
        root.join("units/run/out/native.a"),
        "changed cached native output",
    )
    .unwrap();
    assert_ne!(capture(root, &output, &seal).unwrap().inputs, first.inputs);
}

#[test]
fn actual_manifest_read_is_not_normalized_and_unknown_script_evidence_refuses() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = fixture(root);
    let dep = root.join("units/cutter/out/cutter.d");
    let text = std::fs::read_to_string(&dep).unwrap();
    std::fs::write(
        &dep,
        format!(
            "{} {}\n",
            text.trim_end(),
            root.join("Cargo.toml").display()
        ),
    )
    .unwrap();
    let before = capture(root, &output, &"a".repeat(64)).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.package]\nversion='2.0.0'\n",
    )
    .unwrap();
    assert_ne!(
        capture(root, &output, &"a".repeat(64)).unwrap().inputs,
        before.inputs
    );
    std::fs::write(
        root.join("units/run/fingerprint/run.json"),
        r#"{"local":[{"Unknown":{}}]}"#,
    )
    .unwrap();
    assert!(capture(root, &output, &"a".repeat(64)).is_err());
}

#[test]
fn published_executable_requires_its_producer_identity_when_unit_metadata_is_absent() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = fixture(root);
    std::fs::remove_dir_all(root.join("units/cutter/fingerprint")).unwrap();
    assert!(capture(root, &output, &"a".repeat(64)).is_err());
    let mut rows: Vec<Value> = output
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| aterm_json::from_slice(line).unwrap())
        .collect();
    let row = rows[0].as_object_mut().unwrap();
    row.insert("target".into(), aterm_json::json!({"kind":["bin"]}));
    row.insert(
        "executable".into(),
        root.join("units/cutter/out/cutter")
            .to_string_lossy()
            .as_ref()
            .into(),
    );
    row.insert("trust_unit_identity_sha256".into(), "a".repeat(64).into());
    let output = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let first = capture(root, output.as_bytes(), &"a".repeat(64)).unwrap();
    let observed_other_tree = output.replace(&"a".repeat(64), &"b".repeat(64));
    assert_eq!(
        capture(root, observed_other_tree.as_bytes(), &"a".repeat(64))
            .unwrap()
            .inputs,
        first.inputs
    );
    let bad = output.replace(&"a".repeat(64), "unknown");
    assert!(capture(root, bad.as_bytes(), &"a".repeat(64)).is_err());
}
