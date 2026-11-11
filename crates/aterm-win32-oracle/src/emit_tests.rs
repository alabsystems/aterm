// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn recorded_struct_offsets_must_name_every_source_field_and_no_other() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    for (path, text) in [
        (
            "Cargo.toml",
            "[features]\nWin32 = []\nWin32_Foundation = [\"Win32\"]\n",
        ),
        ("src/core/mod.rs", ""),
        ("src/core/literals.rs", ""),
        (
            "src/Windows/mod.rs",
            "#[cfg(feature = \"Win32\")]\npub mod Win32;\n",
        ),
        (
            "src/Windows/Win32/mod.rs",
            "#[cfg(feature = \"Win32_Foundation\")]\npub mod Foundation;\n",
        ),
        (
            "src/Windows/Win32/Foundation/mod.rs",
            "#[repr(C)]\n#[derive(Clone, Copy)]\npub struct POINT {\n    pub x: i32,\n    pub y: i32,\n}\n",
        ),
    ] {
        let path = scratch.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let up = Upstream::load(scratch.path()).unwrap();
    let variants: Vec<_> = up.items[&(vec!["Win32".into(), "Foundation".into()], "POINT".into())]
        .iter()
        .collect();
    let answers = |offsets: Value| {
        aterm_json::json!({"host":"fixture-64-bit","items":{
            "Win32::Foundation::POINT":{"size":8,"align":4,"offsets":offsets}
        }})
    };
    let complete = answers(aterm_json::json!({"x":0,"y":4}));
    let checks = recorded_tests(&up, &variants, &complete).unwrap();
    assert!(checks.contains("assert!(offset_of!(M::POINT, x) == 0)"));
    assert!(checks.contains("assert!(offset_of!(M::POINT, y) == 4)"));
    let oracle = conformance(&up, &variants, &complete).unwrap();
    assert!(oracle.contains("assert!(offset_of!(U::POINT, x) == 0)"));
    assert!(oracle.contains("assert!(offset_of!(U::POINT, y) == 4)"));

    let mut accepted = Vec::new();
    for (case, offsets) in [
        ("missing y", aterm_json::json!({"x":0})),
        ("unexpected z", aterm_json::json!({"x":0,"y":4,"z":8})),
    ] {
        let data = answers(offsets);
        for (emitter, result) in [
            ("recorded checks", recorded_tests(&up, &variants, &data)),
            ("conformance", conformance(&up, &variants, &data)),
        ] {
            if result.is_ok() {
                accepted.push(format!("{emitter}: {case}"));
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "invalid offsets accepted: {accepted:?}"
    );
}

#[test]
fn record_bootstraps_missing_answers_but_generation_requires_complete_answers() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let upstream = root.join("upstream");
    let files = [
        (
            "Cargo.toml",
            "[features]\nWin32 = []\nWin32_Foundation = [\"Win32\"]\n",
        ),
        ("src/core/mod.rs", ""),
        ("src/core/literals.rs", ""),
        (
            "src/Windows/mod.rs",
            "#[cfg(feature = \"Win32\")]\npub mod Win32;\n",
        ),
        (
            "src/Windows/Win32/mod.rs",
            "#[cfg(feature = \"Win32_Foundation\")]\npub mod Foundation;\n",
        ),
        (
            "src/Windows/Win32/Foundation/mod.rs",
            "pub const ERROR_SUCCESS: WIN32_ERROR = 0u32;\npub type WIN32_ERROR = u32;\n",
        ),
    ];
    for (path, contents) in files {
        let path = upstream.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let up = Upstream::load(&upstream).unwrap();
    let requested = BTreeSet::from(["Win32_Foundation".into()]);
    let enabled = up.implied(&requested).unwrap();
    let model = Model {
        up,
        consumers: BTreeMap::new(),
        direct: BTreeMap::new(),
        enabled,
        requested,
        seeds: BTreeMap::new(),
        keep: BTreeSet::from([
            (
                vec!["Win32".into(), "Foundation".into()],
                "ERROR_SUCCESS".into(),
            ),
            (
                vec!["Win32".into(), "Foundation".into()],
                "WIN32_ERROR".into(),
            ),
        ]),
        notes: vec![],
    };
    for path in [
        "crates/aterm-windows-sys/Cargo.toml",
        "win32-oracle/record/Cargo.toml",
        "win32-oracle/conformance/Cargo.toml",
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            "# BEGIN GENERATED FEATURES\n# END GENERATED FEATURES\n",
        )
        .unwrap();
    }
    let answers = root.join("win32-oracle/recorded.json");
    assert!(!answers.exists());
    let recorder = model
        .recorder_outputs(root)
        .expect("missing answers can be measured");
    let source = &recorder[Path::new("win32-oracle/record/src/main.rs")];
    assert!(source.contains("value\\tWin32::Foundation::ERROR_SUCCESS"));
    assert!(source.contains("U::ERROR_SUCCESS"));
    let manifest = &recorder[Path::new("win32-oracle/record/Cargo.toml")];
    assert!(manifest.contains("version = \"=0.61.2\""));
    assert!(manifest.contains("\"Win32_Foundation\""));
    assert!(
        !answers.exists(),
        "preparation never invents recorded answers"
    );
    assert!(
        model.outputs(root).is_err(),
        "generation still needs answers"
    );

    std::fs::write(
        &answers,
        "{\"windows-sys\":\"0.61.2\",\"host\":\"test-64-bit\",\"items\":{}}",
    )
    .unwrap();
    let incomplete = std::fs::read(&answers).unwrap();
    assert!(
        model
            .outputs(root)
            .unwrap_err()
            .contains("lacks Win32::Foundation::ERROR_SUCCESS")
    );
    assert_eq!(std::fs::read(&answers).unwrap(), incomplete);

    std::fs::write(
        &answers,
        "{\"windows-sys\":\"0.61.2\",\"host\":\"test-64-bit\",\"items\":{\"Win32::Foundation::ERROR_SUCCESS\":{\"value\":\"0\"}}}",
    )
    .unwrap();
    let outputs = model
        .outputs(root)
        .expect("complete measured answers generate all checks");
    assert!(
        outputs[Path::new("crates/aterm-win32/tests/recorded.rs")]
            .contains("assert!(M::ERROR_SUCCESS == 0)")
    );
    assert_eq!(
        outputs[Path::new("win32-oracle/record/src/main.rs")],
        *source
    );
}
