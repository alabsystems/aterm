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
        unit.join("fingerprint/bin-cutter.json"),
        r#"{"rustflags":["-Zthreads=4"],"local":[]}"#,
    )
    .unwrap();
    std::fs::write(
        unit.join("fingerprint/outputs-bin-cutter.json"),
        r#"{"schema":4,"outputs":[]}"#,
    )
    .unwrap();
    let compiler = root.join("units/script");
    std::fs::create_dir_all(compiler.join("out")).unwrap();
    std::fs::create_dir_all(compiler.join("fingerprint")).unwrap();
    std::fs::write(package.join("build.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        compiler.join("out/build_script_build.d"),
        format!("output: {}\n", package.join("build.rs").display()),
    )
    .unwrap();
    std::fs::write(
        compiler.join("fingerprint/build-script-build-script-build.json"),
        r#"{"local":[]}"#,
    )
    .unwrap();
    std::fs::write(
        compiler.join("fingerprint/outputs-build-script-build-script-build.json"),
        r#"{"schema":4,"outputs":[]}"#,
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
        generated.join("fingerprint/run-build-script-build-script-build.json"),
        r#"{"local":[{"RerunIfChanged":{"output":"units/run/run/stdout","paths":["assets"]}}]}"#,
    )
    .unwrap();
    let artifact = aterm_json::json!({
        "reason":"compiler-artifact", "fresh":true, "package_id":"path+file:///fixture#cutter@0.0.0",
        "manifest_path":package.join("Cargo.toml"), "filenames":[unit.join("out/cutter")],
        "target":{"kind":["bin"],"name":"cutter"}, "profile":{"test":false},
    });
    let build_script = aterm_json::json!({
        "reason":"compiler-artifact", "fresh":true, "package_id":"path+file:///fixture#cutter@0.0.0",
        "manifest_path":package.join("Cargo.toml"), "filenames":[compiler.join("out/build_script_build")],
        "target":{"kind":["custom-build"],"name":"build-script-build"}, "profile":{"test":false},
    });
    let script = aterm_json::json!({
        "reason":"build-script-executed", "package_id":"path+file:///fixture#cutter@0.0.0",
        "out_dir": generated.join("out"),
    });
    format!("{artifact}\n{build_script}\n{script}\n").into_bytes()
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
        "units/cutter/fingerprint/bin-cutter.json",
        "units/cutter/fingerprint/outputs-bin-cutter.json",
        "units/run/run/stdout",
    ] {
        let path = root.join(path);
        let original = std::fs::read(&path).unwrap();
        let changed = if path.ends_with("Cargo.lock") {
            String::from_utf8(original.clone())
                .unwrap()
                .replace("dep 1.0.0", "dep 2.0.0")
        } else if path
            .file_name()
            .is_some_and(|name| name == "outputs-bin-cutter.json")
        {
            r#"{"schema":4,"outputs":["changed receipt"]}"#.into()
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
        root.join("units/run/fingerprint/run-build-script-build-script-build.json"),
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
    row.insert(
        "target".into(),
        aterm_json::json!({"kind":["bin"],"name":"cutter"}),
    );
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

fn content_declarations() -> Value {
    aterm_json::json!([
        {"RerunIfChangedContent":{
            "output":"units/run/run/stdout", "paths":["assets"],
            "snapshot":{"version":1,"entries":2,"bytes":29,"sha256":"a".repeat(64)}
        }},
        {"RerunIfEnvChanged":{"var":"BUILD_INPUT","val":"first"}}
    ])
}

fn script_declarations(root: &Path, local: &Value) {
    std::fs::write(
        root.join("units/run/fingerprint/run-build-script-build-script-build.json"),
        aterm_json::json!({"local":local}).to_string(),
    )
    .unwrap();
}

fn declaration_mut<'a>(local: &'a mut Value, pointer: &str) -> &'a mut Value {
    pointer.split('/').skip(1).fold(local, |value, part| {
        if let Ok(index) = part.parse::<usize>() {
            value.as_array_mut().unwrap().get_mut(index).unwrap()
        } else {
            value.as_object_mut().unwrap().get_mut(part).unwrap()
        }
    })
}

#[test]
fn content_snapshots_bind_paths_environment_and_the_complete_record() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = fixture(root);
    let local = content_declarations();
    script_declarations(root, &local);
    let seal = "a".repeat(64);
    let first = capture(root, &output, &seal).unwrap().inputs;
    for (pointer, value) in [
        (
            "/0/RerunIfChangedContent/snapshot/sha256",
            aterm_json::json!("b".repeat(64)),
        ),
        ("/1/RerunIfEnvChanged/val", aterm_json::json!("second")),
        (
            "/0/RerunIfChangedContent/paths",
            aterm_json::json!(["source.rs"]),
        ),
    ] {
        let mut changed = local.clone();
        *declaration_mut(&mut changed, pointer) = value;
        script_declarations(root, &changed);
        assert_ne!(capture(root, &output, &seal).unwrap().inputs, first);
    }
    script_declarations(root, &local);
    std::fs::write(
        root.join("crates/cutter/assets/input"),
        "changed declared content\n",
    )
    .unwrap();
    assert_ne!(capture(root, &output, &seal).unwrap().inputs, first);
    // Snapshot::valid permits zero counters; they are not authority to omit paths.
    let mut zero = local;
    *declaration_mut(&mut zero, "/0/RerunIfChangedContent/snapshot/entries") = aterm_json::json!(0);
    *declaration_mut(&mut zero, "/0/RerunIfChangedContent/snapshot/bytes") = aterm_json::json!(0);
    script_declarations(root, &zero);
    assert!(capture(root, &output, &seal).is_ok());
}

#[test]
fn unavailable_unknown_or_malformed_content_evidence_declines_adoption() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = fixture(root);
    let local = content_declarations();
    for (pointer, value) in [
        ("/0/RerunIfChangedContent/snapshot", Value::Null),
        (
            "/0/RerunIfChangedContent/snapshot/version",
            aterm_json::json!(2),
        ),
        (
            "/0/RerunIfChangedContent/snapshot/entries",
            aterm_json::json!(100_001),
        ),
        (
            "/0/RerunIfChangedContent/snapshot/bytes",
            aterm_json::json!(512_u64 * 1024 * 1024 + 1),
        ),
        (
            "/0/RerunIfChangedContent/snapshot/sha256",
            aterm_json::json!("A".repeat(64)),
        ),
        ("/0/RerunIfChangedContent/paths", aterm_json::json!([false])),
        (
            "/0/RerunIfChangedContent/output",
            aterm_json::json!("another/unit/run/stdout"),
        ),
        ("/0", aterm_json::json!({"UnknownContent":{}})),
    ] {
        let mut changed = local.clone();
        *declaration_mut(&mut changed, pointer) = value;
        script_declarations(root, &changed);
        assert!(
            capture(root, &output, &"a".repeat(64)).is_err(),
            "{pointer}"
        );
    }
    for removed in ["snapshot", "paths", "output"] {
        let mut changed = local.clone();
        declaration_mut(&mut changed, "/0/RerunIfChangedContent")
            .as_object_mut()
            .unwrap()
            .remove(removed);
        script_declarations(root, &changed);
        assert!(
            capture(root, &output, &"a".repeat(64)).is_err(),
            "{removed}"
        );
    }
    for pointer in [
        "/0/RerunIfChangedContent",
        "/0/RerunIfChangedContent/snapshot",
    ] {
        let mut changed = local.clone();
        declaration_mut(&mut changed, pointer)
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), Value::Null);
        script_declarations(root, &changed);
        assert!(capture(root, &output, &"a".repeat(64)).is_err());
    }
}

#[test]
fn selected_fingerprints_and_dep_info_cannot_be_replaced_by_valid_siblings() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = fixture(root);
    let seal = "a".repeat(64);
    let first = capture(root, &output, &seal).unwrap().inputs;
    for selected in [
        "units/cutter/fingerprint/bin-cutter.json",
        "units/run/fingerprint/run-build-script-build-script-build.json",
        "units/cutter/out/cutter.d",
    ] {
        let selected = root.join(selected);
        let sibling =
            selected.with_file_name(if selected.extension().is_some_and(|ext| ext == "d") {
                "unrelated-valid-sibling.d"
            } else {
                "unrelated-valid-sibling.json"
            });
        let original = std::fs::read(&selected).unwrap();
        std::fs::write(&sibling, &original).unwrap();
        std::fs::remove_file(&selected).unwrap();
        assert!(
            capture(root, &output, &seal).is_err(),
            "{}",
            selected.display()
        );
        std::fs::write(&selected, &original).unwrap();
        assert_eq!(capture(root, &output, &seal).unwrap().inputs, first);
        std::fs::remove_file(sibling).unwrap();
    }
    let selected = root.join("units/cutter/fingerprint/bin-cutter.json");
    std::fs::copy(
        root.join("units/cutter/fingerprint/outputs-bin-cutter.json"),
        &selected,
    )
    .unwrap();
    assert!(capture(root, &output, &seal).is_err());
}

#[test]
fn selected_targets_use_literal_names_and_refuse_unbound_script_execution() {
    let row = aterm_json::json!({"profile":{"test":false},"target":{"kind":["rlib","dylib"],"name":"crate-name"}});
    assert_eq!(
        selected_target(&row).unwrap(),
        ("lib-crate-name".into(), false)
    );
    let mut proc_macro = row.clone();
    *declaration_mut(&mut proc_macro, "/target/kind") = aterm_json::json!(["proc-macro"]);
    assert_eq!(selected_target(&proc_macro).unwrap().0, "lib-crate-name");
    for kind in ["unknown", "test", "example"] {
        let mut changed = row.clone();
        *declaration_mut(&mut changed, "/target/kind") = aterm_json::json!([kind]);
        assert!(selected_target(&changed).is_err());
    }
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = String::from_utf8(fixture(root)).unwrap();
    let without_compiler = output
        .lines()
        .filter(|line| !line.contains("custom-build"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(capture(root, without_compiler.as_bytes(), &"a".repeat(64)).is_err());
    let without_execution = output
        .lines()
        .filter(|line| !line.contains("build-script-executed"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(capture(root, without_execution.as_bytes(), &"a".repeat(64)).is_err());
    let mut compiler: Value = aterm_json::from_str(output.lines().nth(1).unwrap()).unwrap();
    *declaration_mut(&mut compiler, "/target/name") = aterm_json::json!("another-script");
    assert!(
        capture(
            root,
            format!("{output}\n{compiler}").as_bytes(),
            &"a".repeat(64)
        )
        .is_err()
    );
}

fn published_library_fixture(root: &Path) -> Vec<u8> {
    let mut output = fixture(root);
    let unit = root.join("units/library");
    let archive = unit.join("out/libcutter_lib-0123456789abcdef.rlib");
    let metadata = archive.with_extension("rmeta");
    let published = root.join("published/libcutter_lib.rlib");
    std::fs::create_dir_all(unit.join("out")).unwrap();
    std::fs::create_dir_all(unit.join("fingerprint")).unwrap();
    std::fs::create_dir_all(published.parent().unwrap()).unwrap();
    std::fs::write(&archive, b"the selected compiler's archive bytes").unwrap();
    std::fs::write(&metadata, b"the selected compiler's metadata bytes").unwrap();
    std::fs::copy(&archive, &published).unwrap();
    for (dep, source) in [
        (
            unit.join("out/cutter_lib-0123456789abcdef.d"),
            "private-library.rs",
        ),
        (published.with_extension("d"), "aggregate-library.rs"),
    ] {
        let source = root.join("crates/cutter").join(source);
        std::fs::write(&source, "pub fn selected() {}\n").unwrap();
        std::fs::write(dep, format!("output: {}\n", source.display())).unwrap();
    }
    std::fs::write(
        unit.join("fingerprint/lib-cutter_lib.json"),
        r#"{"local":[]}"#,
    )
    .unwrap();
    let mut compiler_outputs = Vec::new();
    let mut outputs = Vec::new();
    for (path, emit) in [(&archive, "link"), (&metadata, "metadata")] {
        let path_sha256 =
            aterm_codec::hex::encode_lower(&Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let bytes = std::fs::read(path).unwrap();
        compiler_outputs.push(aterm_json::json!({"path_sha256":path_sha256,"emit":emit}));
        outputs.push(aterm_json::json!({"path_sha256":path_sha256,"size":bytes.len(),"sha256":aterm_codec::hex::encode_lower(&Sha256::digest(&bytes))}));
    }
    std::fs::write(
        unit.join("fingerprint/outputs-lib-cutter_lib.json"),
        aterm_json::json!({
            "schema":4,"compiler":{"mode":"build","workspace":true,"build_script":false},
            "compiler_outputs":compiler_outputs,"outputs":outputs,
        })
        .to_string(),
    )
    .unwrap();
    let row = aterm_json::json!({
        "reason":"compiler-artifact","fresh":true,"package_id":"path+file:///fixture#cutter@0.0.0",
        "manifest_path":root.join("crates/cutter/Cargo.toml"),
        "target":{"kind":["lib"],"name":"cutter_lib"},"profile":{"test":false},
        "trust_compile_mode":"build","trust_compile_kind":"host","trust_unit_identity_sha256":"b".repeat(64),
        "filenames":[published,metadata],"executable":Value::Null,
    });
    output.extend_from_slice(format!("{row}\n").as_bytes());
    output
}

#[test]
fn a_published_library_keeps_its_selected_private_producer_and_both_source_sets() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = published_library_fixture(root);
    let seal = "a".repeat(64);
    let first = capture(root, &output, &seal).unwrap();
    assert!(first.all_fresh);
    for source in ["private-library.rs", "aggregate-library.rs"] {
        let path = root.join("crates/cutter").join(source);
        let original = std::fs::read(&path).unwrap();
        std::fs::write(&path, "pub fn changed() {}\n").unwrap();
        assert_ne!(capture(root, &output, &seal).unwrap().inputs, first.inputs);
        std::fs::write(path, original).unwrap();
    }
}

#[test]
fn a_published_library_refuses_missing_or_mismatched_private_evidence() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = published_library_fixture(root);
    let seal = "a".repeat(64);
    for file in [
        "units/library/fingerprint/lib-cutter_lib.json",
        "units/library/fingerprint/outputs-lib-cutter_lib.json",
        "units/library/out/cutter_lib-0123456789abcdef.d",
        "units/library/out/libcutter_lib-0123456789abcdef.rmeta",
        "units/library/out/libcutter_lib-0123456789abcdef.rlib",
        "published/libcutter_lib.d",
    ] {
        let path = root.join(file);
        let original = std::fs::read(&path).unwrap();
        let sibling = path.with_file_name("unrelated-selected-evidence.json");
        std::fs::write(&sibling, &original).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(capture(root, &output, &seal).is_err(), "{file}");
        std::fs::write(&path, original).unwrap();
        std::fs::remove_file(sibling).unwrap();
    }
    for file in [
        "published/libcutter_lib.rlib",
        "units/library/out/libcutter_lib-0123456789abcdef.rlib",
        "units/library/out/libcutter_lib-0123456789abcdef.rmeta",
    ] {
        let path = root.join(file);
        let original = std::fs::read(&path).unwrap();
        let mut changed = original.clone();
        changed[0] ^= 1;
        std::fs::write(&path, changed).unwrap();
        assert!(capture(root, &output, &seal).is_err(), "{file}");
        std::fs::write(path, original).unwrap();
    }
    let receipt_path = root.join("units/library/fingerprint/outputs-lib-cutter_lib.json");
    let original = std::fs::read(&receipt_path).unwrap();
    for pointer in ["/outputs/0/path_sha256", "/compiler_outputs/0/emit"] {
        let mut receipt: Value = aterm_json::from_slice(&original).unwrap();
        *declaration_mut(&mut receipt, pointer) = aterm_json::json!("different-producer");
        std::fs::write(&receipt_path, receipt.to_string()).unwrap();
        assert!(capture(root, &output, &seal).is_err(), "{pointer}");
    }
    std::fs::write(receipt_path, original).unwrap();
    let mut rows: Vec<Value> = output
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| aterm_json::from_slice(line).unwrap())
        .collect();
    let library = rows.last_mut().unwrap();
    library.as_object_mut().unwrap().insert(
        "filenames".into(),
        aterm_json::json!([root.join("published/libcutter_lib.rlib")]),
    );
    let unbound = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(capture(root, unbound.as_bytes(), &seal).is_err());
}
