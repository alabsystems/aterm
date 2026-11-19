// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::build_files::Packages;
use super::*;

const COMPILE: &str = "36243e83d352b1b2";
const RUN: &str = "6d926d6fc88e9a84";
const OTHER: &str = "1bd14c2f091b6a93";
const EDGE: &str =
    r#"{"deps":[[6742209449257416124,"build_script_make_tables",false,5623058853896079987]]}"#;
const COMPILED_SCRIPT: &str = r#"{"deps":[[6742209449257416124,"build_script_make_tables",false,5701980534829718917]],"local":[]}"#;
const MARKER: &str = "734a97413c1e094e";

fn target(name: &str, kind: &str, crate_type: &str) -> aterm_json::Value {
    aterm_json::json!({"name": name, "kind": [kind], "crate_types": [crate_type]})
}

fn script_target(root: &Path, stem: &str) -> aterm_json::Value {
    let source = root.join("my-package").join(format!("{stem}.rs"));
    write(&source, b"fn main() {}\n");
    aterm_json::json!({
        "name": format!("build-script-{stem}"), "kind": ["custom-build"],
        "crate_types": ["bin"], "src_path": source
    })
}

fn package_row(root: &Path, targets: &[aterm_json::Value]) -> aterm_json::Value {
    aterm_json::json!({
        "name": "my-package", "manifest_path": root.join("my-package/Cargo.toml"), "targets": targets
    })
}

fn packages(root: &Path, targets: &[aterm_json::Value]) -> Packages {
    let row = package_row(root, targets);
    Packages::from_metadata(&aterm_json::json!({"packages": [row]}), root)
}

fn script_package(root: &Path) -> Packages {
    packages(
        root,
        &[
            target("renamed_lib", "lib", "lib"),
            script_target(root, "make-tables"),
        ],
    )
}

fn records_value(
    dep: &Path,
    name: &str,
    value: &str,
    lane: &super::super::LaneEnv,
    packages: &Packages,
    files: &dyn Fn(&str, &Path) -> bool,
) -> Result<bool, String> {
    Ok(refused_reads(dep, [(name, Some(value))], lane, packages, files)?.is_empty())
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn files_row(profile: &Path, output: &Path, source: &str) -> String {
    aterm_json::to_string(&aterm_json::json!({"RerunIfChangedContent": {
        "output": output.strip_prefix(profile.parent().unwrap()).unwrap(),
        "paths": [source],
        "snapshot": {"version":1,"entries":1,"bytes":30,"sha256":"1".repeat(64)}
    }}))
    .unwrap()
}

fn local_script(marker: &Path, local: &str) {
    let name = marker
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("run-build-script-")
        .unwrap();
    write(
        &marker.with_extension("json"),
        COMPILED_SCRIPT
            .replace("build_script_make_tables", &name.replace('-', "_"))
            .replace("\"local\":[]", &format!("\"local\":[{local}]")),
    );
}

fn tracked_files(root: &Path) -> impl Fn(&str, &Path) -> bool {
    let package_dir = root.join("my-package");
    move |package, path| {
        package == "my-package"
            && (path == Path::new("src/lib.rs")
                || path == Path::new("Cargo.toml")
                || path.strip_prefix(&package_dir).is_ok_and(|rel| {
                    rel == Path::new("make-tables.rs") || rel == Path::new("make_tables.rs")
                }))
    }
}

fn compile(profile: &Path, nested: bool) -> PathBuf {
    let name = format!("renamed_lib-{COMPILE}.d");
    let (dep, fingerprints) = if nested {
        let unit = profile.join("build/my-package").join(COMPILE);
        (unit.join("out").join(name), unit.join("fingerprint"))
    } else {
        (
            profile.join("deps").join(name),
            profile.join(format!(".fingerprint/my-package-{COMPILE}")),
        )
    };
    write(&dep, "renamed_lib.rmeta: src/lib.rs\n");
    write(&fingerprints.join("lib-renamed_lib.json"), EDGE);
    dep
}

fn script(
    profile: &Path,
    nested: bool,
    package: &str,
    id: &str,
    marker: &str,
    stdout: &[u8],
) -> (PathBuf, PathBuf) {
    named_script(
        profile,
        nested,
        package,
        id,
        marker,
        stdout,
        "build-script-make-tables",
    )
}

fn named_script(
    profile: &Path,
    nested: bool,
    package: &str,
    id: &str,
    marker: &str,
    stdout: &[u8],
    target: &str,
) -> (PathBuf, PathBuf) {
    let (fingerprints, output) = if nested {
        let unit = profile.join("build").join(package).join(id);
        (unit.join("fingerprint"), unit.join("run/stdout"))
    } else {
        let unit = format!("{package}-{id}");
        (
            profile.join(".fingerprint").join(&unit),
            profile.join("build").join(unit).join("output"),
        )
    };
    let mark = fingerprints.join(format!("run-build-script-{target}"));
    write(&mark, marker);
    local_script(&mark, &files_row(profile, &output, "src/lib.rs"));
    write(&output, stdout);
    let root = output.with_file_name("root-output");
    let record = |path: &Path, bytes: &[u8]| {
        aterm_json::json!({
            "path_sha256": crate::publish::sha256_bytes(path.as_os_str().as_encoded_bytes()),
            "size": bytes.len(),
            "sha256": crate::publish::sha256_bytes(bytes)
        })
    };
    let receipt = aterm_json::json!({
        "schema": 2, "unit": id, "outputs": [record(&output, stdout), record(&root, b"")]
    });
    write(
        &output.with_file_name("output-integrity-v2.json"),
        aterm_json::to_vec(&receipt).unwrap(),
    );
    (mark, output)
}

#[test]
fn build_env_matches_the_consumers_run_and_effective_value_in_both_layouts() {
    let lane = Default::default();
    for nested in [true, false] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let tracked = tracked_files(temp.path());
        let packages = script_package(temp.path());
        let profile = temp.path().join("j1/trust-lane");
        let dep = compile(&profile, nested);
        let output = b"\xff\ncargo::rustc-env=LABEL=earlier\n  targo:rustc-env=LABEL=last=one\\path   \r\ncargo:rustc-env=EMPTY=\ntargo::rustc-env=RUSTC_BOOTSTRAP=1\n";
        let (mark, stdout) = script(&profile, nested, "my-package", RUN, MARKER, output);
        let matches =
            |value| records_value(&dep, "LABEL", value, &lane, &packages, &tracked).unwrap();
        assert!(matches(r"last=one\\path"));
        assert!(!matches("earlier"));
        assert!(!matches(r"last=one\path"));
        assert!(records_value(&dep, "EMPTY", "", &lane, &packages, &tracked).unwrap());
        assert!(!records_value(&dep, "RUSTC_BOOTSTRAP", "1", &lane, &packages, &tracked).unwrap());

        // The ordinary big-endian rendering of the edge, another package, and the
        // correct package in another slot each hold the same stdout but no authority.
        write(&mark, "4e091e3c41974a73");
        script(&profile, nested, "sibling", RUN, MARKER, output);
        script(
            &temp.path().join("j0/trust-lane"),
            nested,
            "my-package",
            RUN,
            MARKER,
            output,
        );
        assert!(!matches(r"last=one\\path"));
        write(&mark, MARKER);
        write(
            &mark.with_extension("json"),
            COMPILED_SCRIPT.replace("6742209449257416124", "22"),
        );
        assert!(!matches(r"last=one\\path"));
        local_script(&mark, &files_row(&profile, &stdout, "src/lib.rs"));
        script(&profile, nested, "my-package", OTHER, MARKER, output);
        assert!(!matches(r"last=one\\path"), "two matching run markers");
    }
}

#[test]
fn dep_info_checks_each_consumers_script_inputs_once_for_all_its_reads() {
    let root = aterm_tempfile::tempdir().unwrap();
    let tracked = tracked_files(root.path());
    let packages = script_package(root.path());
    let profile = root.path().join("j0/debug");
    let dep = compile(&profile, true);
    let (marker, stdout) = script(
        &profile,
        true,
        "my-package",
        RUN,
        MARKER,
        b"cargo:rustc-env=VALUE=measured\n",
    );
    let lane = super::super::LaneEnv::default();
    let checks = std::cell::Cell::new(0usize);
    let covered = |package: &str, path: &Path| {
        checks.set(checks.get() + 1);
        tracked(package, path)
    };
    let check = |rows: &str| {
        write(&dep, rows);
        checks.set(0);
        let result = super::super::dep_info_check(root.path(), &lane, &packages, &covered);
        (checks.get(), result)
    };
    let (once, result) = check("# env-dep:VALUE=measured\n");
    result.unwrap();
    assert!(once > 0);
    let (batch, result) = check(
        "# env-dep:OPTIONAL\n# env-dep:TARGO_PKG_NAME=my-package\n# env-dep:VALUE=measured\n",
    );
    result.unwrap();
    assert_eq!(batch, once);

    let (batch, result) = check(
        "# env-dep:VALUE=undisclosed\n# env-dep:UNKNOWN=undisclosed\n# env-dep:UNKNOWN=again\n",
    );
    let why = result.unwrap_err();
    assert!(why.contains("read of UNKNOWN") && why.contains("1 other variable"));
    assert!(!why.contains("undisclosed") && !why.contains("again"));
    assert_eq!(batch, once);

    let row = aterm_json::json!({"RerunIfChangedContent": {
        "output": stdout.strip_prefix(profile.parent().unwrap()).unwrap(),
        "paths": ["src/lib.rs"],
        "snapshot": null
    }});
    local_script(&marker, &aterm_json::to_string(&row).unwrap());
    let (_, result) = check("# env-dep:OPTIONAL\n# env-dep:VALUE=undisclosed\n");
    let why = result.unwrap_err();
    assert!(
        why.contains("no build-script content snapshot for package my-package"),
        "{why}"
    );
    assert!(!why.contains("OPTIONAL") && !why.contains("undisclosed"));
}

#[test]
fn dep_info_reports_the_uncovered_build_script_input_without_disclosing_its_contents() {
    let root = aterm_tempfile::tempdir().unwrap();
    let profile = root.path().join("j0/debug");
    let tracked = tracked_files(root.path());
    let packages = script_package(root.path());
    let dep = compile(&profile, true);
    write(&dep, "# env-dep:LABEL=private-output\n");
    let lane = super::super::LaneEnv {
        passed: ["HOME".to_owned()].into(),
        ..Default::default()
    };
    let check = || super::super::dep_info_check(root.path(), &lane, &packages, &tracked);
    let supply = |declaration: &str| {
        let stdout = format!("cargo:rustc-env=LABEL=private-output\n{declaration}\n");
        script(&profile, true, "my-package", RUN, MARKER, stdout.as_bytes())
    };
    let (marker, _) = supply("cargo:rerun-if-env-changed=HOME");
    local_script(
        &marker,
        r#"{"RerunIfEnvChanged":{"var":"HOME","val":"private-home-value"}}"#,
    );
    let why = check().unwrap_err();
    assert!(
        why.contains("build script for package my-package")
            && why.contains("host-passed environment variable HOME"),
        "{why}"
    );
    assert!(!why.contains("private-home-value") && !why.contains("private-output"));
    local_script(
        &marker,
        r#"{"RerunIfEnvChanged":{"var":"OTHER","val":null}}"#,
    );
    let why = check().unwrap_err();
    assert!(!why.contains("host-passed") && !why.contains("private-output"));

    let outside = "../private-runtime-input";
    for (path, declaration) in [
        (outside, format!("cargo:rerun-if-changed={outside}")),
        (outside, String::new()),
        ("src", "cargo:rerun-if-changed=src".into()),
    ] {
        let (marker, stdout) = supply(&declaration);
        local_script(&marker, &files_row(&profile, &stdout, path));
        let why = check().unwrap_err();
        assert!(
            why.contains("build script for package my-package")
                && why.contains("file input the proof key does not cover"),
            "{why}"
        );
        assert!(!why.contains("private-runtime-input") && !why.contains("private-output"));
    }
    supply("cargo:rerun-if-changed=src/lib.rs");
    check().unwrap();
}

#[test]
fn compiler_metadata_and_pins_check_the_owning_scripts_effective_setting() {
    let lane = super::super::LaneEnv {
        passed: ["HOME".to_owned()].into(),
        pins: [("SOURCE_DATE_EPOCH".to_owned(), "0".to_owned())].into(),
    };
    for nested in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let tracked = tracked_files(root.path());
        let profile = root.path();
        let packages = script_package(root.path());
        let dep = compile(profile, nested);
        let reads = |key, value| records_value(&dep, key, value, &lane, &packages, &tracked);
        let absent = |key| {
            refused_reads(&dep, [(key, None)], &lane, &packages, &tracked)
                .map(|rows| rows.is_empty())
        };
        let intrinsic = [
            ("CARGO_PKG_NAME", "my-package"),
            ("TARGO_PKG_NAME", "my-package"),
            ("SOURCE_DATE_EPOCH", "0"),
        ];
        for (key, value) in intrinsic {
            assert!(!reads(key, value).unwrap(), "missing script {key}");
        }
        let host = r#"{"RerunIfEnvChanged":{"var":"HOME","val":"/host"}}"#;
        let declaration = "cargo:rerun-if-env-changed=HOME\n";
        let (marker, _) = script(
            profile,
            nested,
            "my-package",
            RUN,
            MARKER,
            declaration.as_bytes(),
        );
        local_script(&marker, host);
        for (key, value) in intrinsic {
            assert!(
                reads(key, value).is_err(),
                "host-dependent script could set {key}"
            );
        }
        assert!(absent("OPTIONAL_LABEL").is_err());
        script(profile, nested, "my-package", RUN, MARKER, b"");
        for (key, value) in intrinsic {
            assert!(
                reads(key, value).unwrap(),
                "covered script did not set {key}"
            );
        }
        assert!(absent("OPTIONAL_LABEL").unwrap());
        assert!(!absent("TARGO_PKG_VERSION").unwrap());
        assert!(!absent("SOURCE_DATE_EPOCH").unwrap());
        for key in [
            "CARGO_PKG_BUILD_LABEL",
            "TARGO_PKG_BUILD_LABEL",
            "CARGO_RUSTC_CURRENT_DIR",
            "CARGO_BIN_EXE_unknown",
        ] {
            assert!(!reads(key, "unbound").unwrap(), "not supplied: {key}");
        }
        let emitted = "cargo:rustc-env=CARGO_PKG_NAME=my-package\ntargo::rustc-env=TARGO_PKG_NAME=my-package\ncargo::rustc-env=SOURCE_DATE_EPOCH=0\ncargo:rustc-env=CARGO_PKG_BUILD_LABEL=value\n";
        script(
            profile,
            nested,
            "my-package",
            RUN,
            MARKER,
            format!("{declaration}{emitted}").as_bytes(),
        );
        local_script(&marker, host);
        for (key, value) in intrinsic {
            assert!(
                reads(key, value).is_err(),
                "host-dependent script overrode {key}"
            );
        }
        assert!(reads("CARGO_PKG_BUILD_LABEL", "value").is_err());
        script(
            profile,
            nested,
            "my-package",
            RUN,
            MARKER,
            emitted.as_bytes(),
        );
        for (key, value) in intrinsic {
            assert!(reads(key, value).unwrap(), "covered script supplied {key}");
        }
        assert!(reads("CARGO_PKG_BUILD_LABEL", "value").unwrap());
    }
}

#[test]
fn script_like_dependency_aliases_do_not_hide_or_impersonate_the_owners_scripts() {
    let root = aterm_tempfile::tempdir().unwrap();
    let tracked = tracked_files(root.path());
    let profile = root.path();
    let dep = compile(profile, true);
    let fingerprint = profile
        .join("build/my-package")
        .join(COMPILE)
        .join("fingerprint/lib-renamed_lib.json");
    let ordinary = r#"[17,"build_script_make_tables",false,42],[23,"metabuild_library",false,96]"#;
    let lane = super::super::LaneEnv {
        passed: ["HOME".to_owned()].into(),
        ..Default::default()
    };
    let pkg_name = |packages: &Packages| {
        records_value(
            &dep,
            "CARGO_PKG_NAME",
            "my-package",
            &lane,
            packages,
            &tracked,
        )
    };
    write(&fingerprint, format!(r#"{{"deps":[{ordinary}]}}"#));
    let no_script = packages(profile, &[target("renamed_lib", "lib", "lib")]);
    assert!(pkg_name(&no_script).unwrap());

    let owner = script_package(profile);
    // A missing run edge, even when a run from an earlier compile is on disk.
    script(
        profile,
        true,
        "my-package",
        RUN,
        MARKER,
        b"cargo:rustc-env=FROM_RUN=7\n",
    );
    assert!(!pkg_name(&owner).unwrap());
    write(&fingerprint, EDGE.replace("[[", &format!("[{ordinary},[")));
    assert!(pkg_name(&owner).unwrap());
    assert!(records_value(&dep, "FROM_RUN", "7", &lane, &owner, &tracked).unwrap());

    let (mark, _) = script(
        profile,
        true,
        "my-package",
        RUN,
        MARKER,
        b"cargo:rerun-if-env-changed=HOME\n",
    );
    local_script(&mark, r#"{"RerunIfEnvChanged":{"var":"HOME","val":null}}"#);
    assert!(pkg_name(&owner).is_err());
    script(profile, true, "my-package", RUN, MARKER, b"");
    script(profile, true, "my-package", OTHER, MARKER, b"");
    assert!(
        !pkg_name(&owner).unwrap(),
        "two runs match the same consumer edge"
    );
}

#[test]
fn all_declared_scripts_must_match_before_metadata_or_script_values_are_accepted() {
    let lane = Default::default();
    let root = aterm_tempfile::tempdir().unwrap();
    let tracked = tracked_files(root.path());
    let profile = root.path();
    let dep = compile(profile, true);
    // Targo keeps distinct raw target names even when rustc normalizes both alike.
    let packages = packages(
        profile,
        &[
            target("renamed_lib", "lib", "lib"),
            script_target(profile, "make-tables"),
            script_target(profile, "make_tables"),
        ],
    );
    let reads = |key, value| records_value(&dep, key, value, &lane, &packages, &tracked).unwrap();
    let value = b"cargo:rustc-env=LABEL=first\n";
    script(profile, true, "my-package", RUN, MARKER, value);
    let second = |output: &[u8]| {
        named_script(
            profile,
            true,
            "my-package",
            OTHER,
            "2a00000000000000",
            output,
            "build-script-make_tables",
        )
    };
    second(value);
    assert!(!reads("CARGO_PKG_NAME", "my-package"));
    assert!(!reads("LABEL", "first"));
    let fingerprint = profile
        .join("build/my-package")
        .join(COMPILE)
        .join("fingerprint/lib-renamed_lib.json");
    write(
        &fingerprint,
        EDGE.replace(
            "]]",
            r#"],[6742209449257416124,"build_script_make_tables",false,42]]"#,
        ),
    );
    assert!(reads("CARGO_PKG_NAME", "my-package"));
    assert!(reads("LABEL", "first"));
    second(b"cargo:rustc-env=LABEL=second\n");
    assert!(!reads("LABEL", "first"));
    assert!(!reads("LABEL", "second"));
}

#[test]
fn executable_examples_get_their_exact_binary_name_from_package_targets() {
    let root = aterm_tempfile::tempdir().unwrap();
    let tracked = tracked_files(root.path());
    let unit = root.path().join("debug/build/my-package").join(COMPILE);
    let dep = unit.join("out/draw_lines.d");
    write(&dep, "draw_lines: examples/draw-lines.rs\n");
    write(
        &unit.join("fingerprint/example-draw-lines.json"),
        br#"{"deps":[]}"#,
    );
    let bin_target = target("draw-lines", "example", "bin");
    let binary = packages(root.path(), std::slice::from_ref(&bin_target));
    let library = packages(root.path(), &[target("draw-lines", "example", "rlib")]);
    let row = package_row(root.path(), &[bin_target]);
    let duplicate = Packages::from_metadata(
        &aterm_json::json!({"packages":[row.clone(),row]}),
        root.path(),
    );
    for key in ["CARGO_BIN_NAME", "TARGO_BIN_NAME"] {
        let accepts = |p: &Packages, value| {
            refused_reads(&dep, [(key, value)], &Default::default(), p, &tracked)
                .unwrap()
                .is_empty()
        };
        assert!(accepts(&binary, Some("draw-lines")));
        assert!(!accepts(&binary, Some("draw_lines")));
        assert!(!accepts(&binary, None));
        assert!(accepts(&library, None));
        for p in [&library, &duplicate, &Packages::default()] {
            assert!(!accepts(p, Some("draw-lines")));
        }
    }
}

#[test]
fn an_unhashed_build_script_compile_reads_its_own_package_metadata() {
    let lane = Default::default();
    for nested in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let tracked = tracked_files(root.path());
        let profile = root.path();
        let packages = script_package(profile);
        let (out, fingerprints) = if nested {
            let unit = profile.join("build/my-package").join(COMPILE);
            (unit.join("out"), unit.join("fingerprint"))
        } else {
            let unit = format!("my-package-{COMPILE}");
            (
                profile.join("build").join(&unit),
                profile.join(".fingerprint").join(unit),
            )
        };
        let dep = out.join("build_script_make_tables.d");
        write(&dep, "build_script_make_tables: build.rs\n");
        let target = "build-script-build-script-make-tables.json";
        // A build-dependency may have exactly the name of this package's script.
        write(
            &fingerprints.join(target),
            br#"{"deps":[[17,"build_script_make_tables",false,42]]}"#,
        );
        script(
            profile,
            nested,
            "my-package",
            RUN,
            MARKER,
            b"cargo:rustc-env=ONLY_FROM_RUN=7\n",
        );
        let reads =
            |key, value| records_value(&dep, key, value, &lane, &packages, &tracked).unwrap();
        assert!(reads("TARGO_PKG_NAME", "my-package"));
        assert!(reads("CARGO_CRATE_NAME", "build_script_make_tables"));
        assert!(!reads("ONLY_FROM_RUN", "7"));
        assert!(
            refused_reads(&dep, [("ONLY_FROM_RUN", None)], &lane, &packages, &tracked)
                .unwrap()
                .is_empty()
        );
        assert!(!reads("CARGO_PKG_NAME", "sibling"));
        assert!(!reads("TARGO_BIN_NAME", "build_script_make_tables"));
        assert!(!reads("CARGO_MANIFEST_FAKE", "/host"));
        fs::remove_file(fingerprints.join(target)).unwrap();
        assert!(!reads("TARGO_PKG_NAME", "my-package"));
    }
}

#[test]
fn a_covered_run_cannot_supply_values_from_an_unkeyed_script_source() {
    let root = aterm_tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let tracked = tracked_files(&repo);
    let profile = repo.join("target/debug");
    let dep = compile(&profile, true);
    script(
        &profile,
        true,
        "my-package",
        RUN,
        MARKER,
        b"cargo:rerun-if-changed=src/lib.rs\ncargo:rustc-env=LABEL=generated\n",
    );
    let read = |script| {
        let packages = packages(&repo, &[target("renamed_lib", "lib", "lib"), script]);
        records_value(
            &dep,
            "LABEL",
            "generated",
            &Default::default(),
            &packages,
            &tracked,
        )
    };
    assert!(read(script_target(&repo, "make-tables")).unwrap());
    for source in [
        script_target(root.path(), "make-tables"), // Identical file outside the keyed tree.
        target("build-script-make-tables", "custom-build", "bin"), // Unavailable source path.
    ] {
        let why = read(source).unwrap_err();
        assert!(
            why.contains("build-script source") && why.contains("my-package"),
            "{why}"
        );
    }
}

#[test]
fn build_env_checks_the_run_receipt_before_trusting_equal_length_stdout() {
    let lane = Default::default();
    for nested in [true, false] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let tracked = tracked_files(temp.path());
        let packages = script_package(temp.path());
        let dep = compile(temp.path(), nested);
        let (_, output) = script(
            temp.path(),
            nested,
            "my-package",
            RUN,
            MARKER,
            b"cargo::rustc-env=K=A\n",
        );
        let matches = |value| records_value(&dep, "K", value, &lane, &packages, &tracked).unwrap();
        assert!(matches("A"));
        fs::write(&output, b"cargo::rustc-env=K=B\n").unwrap();
        assert!(
            !matches("B"),
            "same path, size, run and fingerprint with new bytes"
        );
        fs::write(&output, b"cargo::rustc-env=K=A\n").unwrap();
        let receipt = output.with_file_name("output-integrity-v2.json");
        let original = fs::read_to_string(&receipt).unwrap();
        let digest =
            |path: &Path| crate::publish::sha256_bytes(path.as_os_str().as_encoded_bytes());
        for wrong in [
            original.replace(RUN, OTHER),
            original.replace(&digest(&output), &digest(&temp.path().join("elsewhere"))),
            original.replace(
                &digest(&output.with_file_name("root-output")),
                &digest(&output),
            ),
        ] {
            fs::write(&receipt, wrong).unwrap();
            assert!(!matches("A"));
        }
        fs::remove_file(&receipt).unwrap();
        assert!(!matches("A"));
    }
}

#[test]
fn build_env_requires_the_supplying_scripts_declared_inputs_to_be_covered() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let tracked = tracked_files(temp.path());
    let packages = script_package(temp.path());
    let dep = compile(temp.path(), true);
    let lane = super::super::LaneEnv {
        passed: ["HOME".to_owned()].into(),
        pins: [("SOURCE_DATE_EPOCH".to_owned(), "0".to_owned())].into(),
    };
    let observed = |var: &str, val: Option<&str>| {
        aterm_json::to_string(&aterm_json::json!({"RerunIfEnvChanged": {"var":var, "val":val}}))
            .unwrap()
    };
    let check = |directive: &str, local: &str| {
        let output = format!("targo::rustc-env=LABEL=literal\n{directive}\n");
        let (marker, _) = script(
            temp.path(),
            true,
            "my-package",
            RUN,
            MARKER,
            output.as_bytes(),
        );
        local_script(&marker, local);
        records_value(&dep, "LABEL", "literal", &lane, &packages, &tracked)
    };
    let env = |var: &str, val: Option<&str>| {
        check(
            &format!("cargo:rerun-if-env-changed={var}"),
            &observed(var, val),
        )
    };
    assert!(env("HOME", Some("private-value")).is_err());
    assert!(env("HOME", None).is_err());
    assert!(!env("UNKEYED", Some("private-value")).unwrap());
    assert!(!env("SOURCE_DATE_EPOCH", Some("42")).unwrap());
    assert!(!env("SOURCE_DATE_EPOCH", None).unwrap());
    assert!(env("SOURCE_DATE_EPOCH", Some("0")).unwrap());
    assert!(env("CLEARED_PROJECT_FLAG", None).unwrap());
    assert!(!env("NUM_JOBS", None).unwrap()); // Targo sets it when starting the script.
    assert!(!env("TARGO_MANIFEST_DIR", None).unwrap()); // The native twin is also set in the child's environment.
    let declared = "targo:rerun-if-env-changed=CLEARED_PROJECT_FLAG";
    let observation = observed("CLEARED_PROJECT_FLAG", None);
    assert!(!check(declared, "").unwrap());
    assert!(!check("", &observation).unwrap());
    assert!(!check(declared, &format!("{observation},{observation}")).unwrap());
    assert!(check(declared, &observed("CLEARED_PROJECT_FLAG", None)).unwrap());
    assert!(
        !records_value(&dep, "LABEL", "literal", &lane, &packages, &|_, _| false).unwrap(),
        "an environment-only inventory still needs a keyed package"
    );
    let malformed = check(
        declared,
        r#"{"RerunIfEnvChanged":{"var":"CLEARED_PROJECT_FLAG"}}"#,
    );
    assert!(
        !malformed.unwrap(),
        "a missing observation is distinct from an explicit null"
    );
    let stdout = temp
        .path()
        .join("build/my-package")
        .join(RUN)
        .join("run/stdout");
    let files = files_row(temp.path(), &stdout, "src/lib.rs");
    assert!(check("", &files).unwrap(), "Targo's default package files");
    assert!(check("cargo:rerun-if-changed=src/lib.rs", &files).unwrap());
    assert!(!check("cargo:rerun-if-changed=src/lib.rs", "").unwrap());
    assert!(!check("", "").unwrap());
    assert!(!check("", r#"{"Precalculated":"unknown provenance"}"#).unwrap());
    assert!(!check("", &files.replace("\"version\":1", "\"version\":0")).unwrap());
    for path in super::super::FILES_OUTSIDE_TREE_KEY {
        assert!(check(&format!("cargo::rerun-if-changed=../../{path}"), &files).is_err());
        assert!(check("", &files_row(temp.path(), &stdout, path)).is_err());
    }
}

#[test]
fn build_env_refuses_linked_run_output_and_fingerprint_files() {
    let lane = Default::default();
    use std::os::unix::fs::symlink;
    for nested in [true, false] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let tracked = tracked_files(temp.path());
        let packages = script_package(temp.path());
        let profile = temp.path();
        let dep = compile(profile, nested);
        let (marker, stdout) = script(
            profile,
            nested,
            "my-package",
            RUN,
            MARKER,
            b"cargo:rustc-env=K=V\n",
        );
        let receipt = stdout.with_file_name("output-integrity-v2.json");
        for evidence in [&stdout, &marker, &receipt] {
            let saved = profile.join("saved");
            fs::rename(evidence, &saved).unwrap();
            symlink(&saved, evidence).unwrap();
            assert!(records_value(&dep, "K", "V", &lane, &packages, &tracked).is_err());
            fs::remove_file(evidence).unwrap();
            fs::rename(saved, evidence).unwrap();
            assert!(records_value(&dep, "K", "V", &lane, &packages, &tracked).unwrap());
        }
    }
}
