// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Portable half of Trust's `trust.sdk-frontend-component.v2` contract.
//!
//! This validates an explicitly selected, already qualified raw seal. Private
//! preparation/Git inputs are represented by its existing producer attestation;
//! recorded absolute evidence paths are never opened. Transformed packages use
//! the ordinary final LINEAGE and signed tree, not this raw-seal validator.

use std::collections::BTreeSet;
use std::path::{Component, Path};

use aterm_json::{Map, Value as V};

use super::component::{
    LIMIT, Unique, absolute, bytes, digest, document, fields, hash, need, oid, reference, text,
};

const NAME: &str = "targo-trust";
const DETAILS: &str = "FRONTEND-COMPONENT.evidence";
const WP: [&str; 5] = [
    "trust-wp",
    "trust-wp-driver",
    "trust-wp-rustc",
    "targo-trust-wp",
    "cargo-trust-wp",
];
const SOURCES: [&str; 15] = [
    "crates/trust-resource-controller/Cargo.toml",
    "crates/trust-resource-controller/Cargo.lock",
    "crates/trust-resource-controller/examples/probe-scheduler-control.rs",
    "crates/trust-resource-controller/src/client.rs",
    "crates/trust-resource-controller/src/client/sdk.rs",
    "crates/trust-resource-controller/src/client/probe_scheduler.rs",
    "targo-trust/Cargo.toml",
    "targo-trust/src/main.rs",
    "targo-trust/src/pipeline/probe.rs",
    "targo-trust/src/probe_scheduler.rs",
    "targo-trust/src/tool_identity.rs",
    "scripts/tests/doctor_probe_custody_test.py",
    "docs/DECISION_QUEUE.md",
    "crates/trust-coordinator/src/coordinator.rs",
    "targo-trust/src/pipeline/run.rs",
];
const CONTROLS: [&str; 4] = [
    "test_old_plain_child_refuses_then_real_explicit_pipe_succeeds",
    "test_nested_original_gnu_transport_stays_live",
    "test_sdk_present_unrelated_sentinel_absent_and_parent_flags_unchanged",
    "test_malformed_or_partial_gnu_and_sdk_fail_before_probe",
];
const FEATURES: &str =
    "trust-mc-in-process,trust-wp-in-process,trust-vc-in-process,ay-backend,ay-certify";
type R<T> = Result<T, String>;

pub(super) struct Binding {
    pub(super) original: String,
    pub(super) replacement: String,
    pub(super) mint: String,
}

fn copied(root: &Path, name: &str, expected: &str) -> R<V> {
    document(&root.join(DETAILS).join(name), Some(expected))
}

fn phase(value: &V, name: &str, argv: bool) -> R<()> {
    fields(
        value,
        if argv {
            "name exit argv stdout stderr"
        } else {
            "name exit stdout stderr"
        },
    )?;
    need(
        value["name"].as_str() == Some(name) && value["exit"].as_i64() == Some(0),
        "component phase is not typed actual0",
    )?;
    reference(&value["stdout"])?;
    reference(&value["stderr"])?;
    Ok(())
}

fn native_image(raw: &[u8], host: &str) -> R<Vec<String>> {
    let cpu = match host {
        "aarch64-apple-darwin" => 0x0100_000c,
        "x86_64-apple-darwin" => 0x0100_0007,
        _ => return Err("unsupported frontend image host".to_owned()),
    };
    let word = |at: usize| -> R<usize> {
        let end = at.checked_add(4).ok_or("image offset overflow")?;
        let part = raw.get(at..end).ok_or("truncated frontend image word")?;
        let value = u32::from_le_bytes(part.try_into().map_err(|_| "invalid image word")?);
        usize::try_from(value).map_err(|_| "image word does not fit host".to_owned())
    };
    need(
        raw.len() >= 32 && raw.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]),
        "frontend is not thin little-endian Mach-O64",
    )?;
    need(
        word(4)? == cpu && word(12)? == 2,
        "frontend native architecture/type differs",
    )?;
    let count = word(16)?;
    let size = word(20)?;
    need(
        (1..=4096).contains(&count) && size >= 8 && size <= raw.len() - 32,
        "frontend load-command bounds invalid",
    )?;
    let end = 32 + size;
    let mut offset = 32;
    let mut libraries = Vec::new();
    for _ in 0..count {
        need(
            offset <= end.saturating_sub(8),
            "frontend load command truncated",
        )?;
        let command = word(offset)? & 0x7fff_ffff;
        let length = word(offset + 4)?;
        let next = offset.checked_add(length).ok_or("load command overflow")?;
        need(
            length >= 8 && length % 8 == 0 && next <= end,
            "frontend load-command size invalid",
        )?;
        if matches!(command, 0xc | 0x18 | 0x1f | 0x20 | 0x23 | 0xe | 0x27) {
            need(length >= 12, "frontend string command truncated")?;
            let start = word(offset + 8)?;
            let minimum = if matches!(command, 0xc | 0x18 | 0x1f | 0x20 | 0x23) {
                24
            } else {
                12
            };
            need(
                start >= minimum && start < length,
                "frontend native string offset invalid",
            )?;
            let data = &raw[offset + start..next];
            let zero = data
                .iter()
                .position(|b| *b == 0)
                .ok_or("unterminated frontend string")?;
            let name =
                std::str::from_utf8(&data[..zero]).map_err(|_| "frontend string is not UTF-8")?;
            need(command != 0x27, "frontend embeds loader environment")?;
            if command == 0xe {
                need(name == "/usr/lib/dyld", "non-system frontend loader")?;
            } else {
                need(
                    (name.starts_with("/usr/lib/")
                        || name.starts_with("/System/Library/Frameworks/"))
                        && !name.split('/').any(|part| matches!(part, "." | ".."))
                        && !name.contains("//"),
                    "frontend links non-system library",
                )?;
                libraries.push(name.to_owned());
            }
        }
        offset = next;
    }
    need(
        offset == end && !libraries.is_empty(),
        "frontend load-command census invalid",
    )?;
    Ok(libraries)
}

fn hashed_map(value: &V, allowed: Option<&[&str]>) -> R<()> {
    let map = value.as_object().ok_or("component hash map absent")?;
    need(!map.is_empty(), "component hash map empty")?;
    for (name, value) in map {
        if let Some(allowed) = allowed {
            need(
                allowed.contains(&name.as_str()),
                "component source readset differs",
            )?;
        }
        digest(value)?;
    }
    Ok(())
}

fn shape(record: &V, compiler: &str) -> R<()> {
    fields(
        record,
        "schema status name base source build_receipt artifact transport_qualification identity_qualification retained_inputs_unchanged",
    )?;
    need(
        record["schema"].as_str() == Some("trust.sdk-frontend-component.v2")
            && record["status"].as_str() == Some("prepared")
            && record["name"].as_str() == Some(NAME)
            && record["retained_inputs_unchanged"].as_bool() == Some(true),
        "wrong prepared frontend identity",
    )?;
    let base = &record["base"];
    fields(
        base,
        "compiler_source host compiler_sha256 original_stage original_mint accepted_stage_provenance original_targo_trust_sha256 existing_tool_sha256",
    )?;
    need(
        oid(&base["compiler_source"])? == compiler,
        "component compiler source differs",
    )?;
    digest(&base["compiler_sha256"])?;
    digest(&base["original_targo_trust_sha256"])?;
    let host = text(base, "host")?;
    need(
        host.split('-').count() >= 2
            && host.split('-').all(|part| {
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            }),
        "component host malformed",
    )?;
    for value in [
        &base["original_mint"],
        &base["accepted_stage_provenance"],
        &record["build_receipt"],
        &record["transport_qualification"],
    ] {
        reference(value)?;
    }
    let tools = &base["existing_tool_sha256"];
    hashed_map(tools, None)?;
    for name in tools.as_object().ok_or("missing original tools")?.keys() {
        need(
            !name.is_empty()
                && name != "."
                && name != ".."
                && !name.contains('/')
                && !name.contains('\\'),
            "component tool name invalid",
        )?;
    }
    need(
        tools[NAME] == base["original_targo_trust_sha256"]
            && tools["trustc"] == base["compiler_sha256"],
        "original tool identities differ",
    )?;
    let source = &record["source"];
    fields(source, "root commit base tree files")?;
    oid(&source["commit"])?;
    oid(&source["tree"])?;
    need(
        oid(&source["base"])? == compiler,
        "repair source base differs",
    )?;
    hashed_map(&source["files"], Some(&SOURCES))?;
    let artifact = &record["artifact"];
    fields(artifact, "path sha256 bytes system_libraries")?;
    absolute(&artifact["path"])?;
    digest(&artifact["sha256"])?;
    need(
        artifact["bytes"]
            .as_u64()
            .is_some_and(|n| n > 0 && n <= 1 << 30),
        "component artifact length invalid",
    )?;
    let libraries = artifact["system_libraries"]
        .as_array()
        .ok_or("component libraries absent")?;
    let unique: BTreeSet<_> = libraries.iter().filter_map(V::as_str).collect();
    need(
        !libraries.is_empty() && unique.len() == libraries.len(),
        "component native libraries duplicated or malformed",
    )?;
    for name in unique {
        need(
            (name.starts_with("/usr/lib/") || name.starts_with("/System/Library/Frameworks/"))
                && !Path::new(name)
                    .components()
                    .any(|p| p == Component::ParentDir),
            "component non-system library",
        )?;
    }
    Ok(())
}

fn compiler_shape(value: &V) -> R<()> {
    fields(value, "path sha256 source sysroot")?;
    absolute(&value["path"])?;
    absolute(&value["sysroot"])?;
    digest(&value["sha256"])?;
    oid(&value["source"])?;
    Ok(())
}

fn receipts(record: &V, built: &V, controls: &V) -> R<()> {
    fields(
        built,
        "source_commit compiler target argv environment context compiler_version_phase retained_compiler_version_phase version_phase image_phase generated_inputs target_directory lock_sha256 artifact_sha256 retained_inputs_unchanged phases",
    )?;
    need(
        built["source_commit"] == record["source"]["commit"]
            && built["target"] == record["base"]["host"]
            && built["artifact_sha256"] == record["artifact"]["sha256"]
            && built["retained_inputs_unchanged"].as_bool() == Some(true),
        "copied build identity differs",
    )?;
    let argv = built["argv"].as_array().ok_or("build argv absent")?;
    let first = argv.first().ok_or("build argv empty")?;
    absolute(first)?;
    let manifest = Path::new(text(&record["source"], "root")?).join("targo-trust/Cargo.toml");
    need(
        built["argv"]
            == aterm_json::json!([
                first,
                "--unverified",
                "build",
                "--release",
                "--locked",
                "--offline",
                "--manifest-path",
                manifest.to_string_lossy().into_owned(),
                "--features",
                FEATURES,
                "--bin",
                NAME
            ]),
        "frontend recipe differs from canonical five-feature build",
    )?;
    reference(&built["context"])?;
    compiler_shape(&built["compiler"])?;
    phase(
        &built["compiler_version_phase"],
        "build-compiler-version",
        true,
    )?;
    need(
        built["compiler_version_phase"]["argv"]
            == aterm_json::json!([&built["compiler"]["path"], "-vV"]),
        "build compiler version argv differs",
    )?;
    phase(
        &built["retained_compiler_version_phase"],
        "retained-compiler-version",
        true,
    )?;
    need(
        built["retained_compiler_version_phase"]["argv"]
            == aterm_json::json!([
                Path::new(text(&record["base"], "original_stage")?)
                    .join("bin/trustc")
                    .to_string_lossy()
                    .into_owned(),
                "-vV"
            ]),
        "retained compiler observation argv differs",
    )?;
    let environment = built["environment"]
        .as_object()
        .ok_or("frontend environment absent")?;
    need(
        built["environment"]["CFG_VER_HASH"] == record["source"]["commit"]
            && built["environment"]["CFG_TRUST_ORACLE_HOST"].as_str() == Some("1")
            && built["environment"]["CFG_TRUST_ORACLE_COMPILER_HASH"]
                == record["base"]["compiler_source"]
            && built["environment"]["CLEAN_OLEAN_REQUIRE_PINNED_LEAN"].as_str() == Some("1"),
        "canonical identity/Lean environment missing",
    )?;
    absolute(&built["environment"]["ELAN_HOME"])?;
    need(
        valid_release(text(&built["environment"], "CFG_RELEASE")?),
        "retained compiler build release is malformed",
    )?;
    for (key, value) in environment {
        let cfg = key.strip_prefix("CFG_").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        });
        need(
            (cfg || [
                "CLEAN_OLEAN_REQUIRE_PINNED_LEAN",
                "CLEAN_OLEAN_PINNED_LEAN_LIB",
                "ELAN_HOME",
                "LEAN_CC",
                "LZMA_API_STATIC",
                "PKG_CONFIG",
            ]
            .contains(&key.as_str()))
                && value.as_str().is_some_and(|v| !v.is_empty()),
            "canonical environment malformed",
        )?;
    }
    for key in ["CLEAN_OLEAN_PINNED_LEAN_LIB", "PKG_CONFIG"] {
        if environment.contains_key(key) {
            absolute(&built["environment"][key])?;
        }
    }
    if environment.contains_key("LZMA_API_STATIC") {
        need(
            built["environment"]["LZMA_API_STATIC"].as_str() == Some("1"),
            "static native policy differs",
        )?;
    }
    phase(&built["image_phase"], "artifact-code-signature", true)?;
    need(
        built["image_phase"]["argv"]
            == aterm_json::json!([
                "/usr/bin/codesign",
                "--verify",
                "--strict",
                &record["artifact"]["path"]
            ]),
        "signature inspection argv differs",
    )?;
    phase(&built["version_phase"], "frontend-version", false)?;
    fields(
        controls,
        "source_commit source_tree source_relation witness controls phases",
    )?;
    oid(&controls["source_commit"])?;
    oid(&controls["source_tree"])?;
    let witness = &controls["witness"];
    fields(
        witness,
        "path sha256 compiler features source_files target argv lock_sha256",
    )?;
    digest(&witness["sha256"])?;
    compiler_shape(&witness["compiler"])?;
    need(
        witness["target"] == record["base"]["host"]
            && witness["features"] == aterm_json::json!(["probe-scheduler"]),
        "transport witness feature/target differs",
    )?;
    for value in [built, witness] {
        digest(&value["lock_sha256"])?;
        let args = value["argv"].as_array().ok_or("witness argv absent")?;
        need(
            !args.is_empty()
                && args
                    .iter()
                    .all(|s| s.as_str().is_some_and(|s| !s.is_empty()))
                && args.iter().any(|s| s.as_str() == Some("--locked"))
                && args.iter().any(|s| s.as_str() == Some("--offline")),
            "build/witness recipe is not locked offline",
        )?;
    }
    let names = controls["controls"]
        .as_array()
        .ok_or("transport controls absent")?;
    need(
        names.len() == CONTROLS.len()
            && names.iter().filter_map(V::as_str).collect::<BTreeSet<_>>()
                == CONTROLS.into_iter().collect(),
        "required transport controls differ",
    )?;
    for (value, name) in [
        (built, "frontend-build"),
        (controls, "probe-transport-controls"),
    ] {
        let phases = value["phases"].as_array().ok_or("actual phases absent")?;
        need(phases.len() == 1, "actual phase census differs")?;
        phase(&phases[0], name, false)?;
    }
    Ok(())
}

fn canonical_hash(value: &V) -> R<String> {
    aterm_json::to_vec(value)
        .map(|raw| hash(&raw))
        .map_err(|e| e.to_string())
}

fn producer_validation(record: &V, built: &V, controls: &V, identity: &V) -> R<V> {
    let relation = &controls["source_relation"];
    let relation_sha = if relation.is_null() {
        V::Null
    } else {
        V::from(reference(relation)?)
    };
    digest(&identity["subject_artifact"]["sha256"])?;
    digest(&identity["harness"]["sha256"])?;
    Ok(aterm_json::json!({
        "schema":"trust.sdk-frontend-producer-validation.v2", "status":"prepared-validated",
        "source":&record["source"]["commit"], "source_tree":&record["source"]["tree"],
        "source_readset_sha256":canonical_hash(&record["source"]["files"])?,
        "witness_source":&controls["source_commit"], "witness_tree":&controls["source_tree"],
        "witness_readset_sha256":canonical_hash(&controls["witness"]["source_files"])?,
        "source_relation_sha256":relation_sha, "build_receipt_sha256":&record["build_receipt"]["sha256"],
        "build_context_sha256":&built["context"]["sha256"],
        "identity_witness_sha256":&record["identity_qualification"]["result"]["sha256"],
        "identity_source_review_sha256":&record["identity_qualification"]["source_review"]["sha256"],
        "identity_observed_artifact_sha256":&identity["subject_artifact"]["sha256"],
        "identity_harness_sha256":&identity["harness"]["sha256"],
        "identity_scope":"Same-source/configuration witness; not execution of replacement bytes or post-install doctor qualification.",
        "transport_receipt_sha256":&record["transport_qualification"]["sha256"],
        "generated_inputs":&built["generated_inputs"], "build_lock_sha256":&built["lock_sha256"],
        "witness_lock_sha256":&controls["witness"]["lock_sha256"],
        "compiler_source":&record["base"]["compiler_source"], "compiler_sha256":&record["base"]["compiler_sha256"],
        "retained_compiler_release":&built["environment"]["CFG_RELEASE"],
        "retained_compiler_version_sha256":&built["retained_compiler_version_phase"]["stdout"]["sha256"],
        "original_frontend_sha256":&record["base"]["original_targo_trust_sha256"],
        "replacement_sha256":&record["artifact"]["sha256"], "original_mint_sha256":&record["base"]["original_mint"]["sha256"],
        "accepted_stage_provenance_sha256":&record["base"]["accepted_stage_provenance"]["sha256"],
        "scope":"Producer source/recipe validation attestation, not independently repeated private-source validation or runtime qualification."
    }))
}

fn stream(root: &Path, local: &str, reference_value: &V) -> R<Vec<u8>> {
    let expected = reference(reference_value)?;
    let raw = bytes(&root.join(DETAILS).join(local), LIMIT)?;
    need(hash(&raw) == expected, "copied component stream differs")?;
    Ok(raw)
}

fn version(raw: &[u8], source: &str, oracle: Option<&str>, release: Option<&str>) -> R<()> {
    let value = std::str::from_utf8(raw).map_err(|_| "component version is not UTF-8")?;
    let lines: Vec<_> = value.lines().collect();
    if oracle.is_some() {
        need(
            lines
                .first()
                .is_some_and(|line| line.starts_with("targo-trust ")),
            "frontend version identity absent",
        )?;
    } else {
        need(
            lines.contains(&"binary: trustc"),
            "build compiler observation lacks binary: trustc",
        )?;
    }
    let key = if oracle.is_some() {
        "trust-repo-commit-hash:"
    } else {
        "commit-hash:"
    };
    need(
        lines
            .iter()
            .filter(|line| line.starts_with(key))
            .copied()
            .collect::<Vec<_>>()
            == [format!("{key} {source}")],
        "component version source differs",
    )?;
    if let Some(oracle) = oracle {
        need(
            lines
                .iter()
                .filter(|line| line.starts_with("trust-oracle-compiler-commit-hash:"))
                .copied()
                .collect::<Vec<_>>()
                == [format!("trust-oracle-compiler-commit-hash: {oracle}")],
            "component oracle compiler differs",
        )?;
    }
    if let Some(release) = release {
        need(
            lines
                .iter()
                .filter(|line| line.starts_with("trust-oracle-compiler-release:"))
                .copied()
                .collect::<Vec<_>>()
                == [format!("trust-oracle-compiler-release: {release}")],
            "component oracle release differs",
        )?;
    }
    Ok(())
}

fn valid_release(value: &str) -> bool {
    let (core, suffix) = value
        .split_once('-')
        .map_or((value, None), |(core, suffix)| (core, Some(suffix)));
    let parts: Vec<_> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
        && suffix.is_none_or(|s| {
            s.bytes().next().is_some_and(|c| c.is_ascii_alphanumeric())
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
        })
}

fn retained_release<'a>(raw: &'a [u8], source: &str) -> R<&'a str> {
    version(raw, source, None, None)?;
    let text =
        std::str::from_utf8(raw).map_err(|_| "retained compiler observation is not UTF-8")?;
    let binaries: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("binary:"))
        .collect();
    need(
        binaries == ["binary: trustc"],
        "retained compiler binary identity absent or duplicate",
    )?;
    let values: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("release:"))
        .collect();
    let release = values
        .first()
        .and_then(|line| line.strip_prefix("release: "));
    need(
        values.len() == 1 && release.is_some_and(valid_release),
        "retained compiler release absent, duplicate or malformed",
    )?;
    release.ok_or_else(|| "retained compiler release absent".to_owned())
}

fn tool_map(root: &Path) -> R<V> {
    let bin = root.join("bin");
    need(
        std::fs::symlink_metadata(&bin).is_ok_and(|s| s.is_dir() && !s.file_type().is_symlink()),
        "component bin directory is not physical",
    )?;
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&bin).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            paths.push(entry.path());
        }
    }
    let helper = root.join("libexec/trust-analyzer-proc-macro-srv");
    if std::fs::symlink_metadata(&helper).is_ok() {
        paths.push(helper);
    }
    let mut map = Map::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("tool name is not UTF-8")?;
        let value = hash(&bytes(&path, 1 << 30)?);
        need(
            map.insert(name.to_owned(), V::from(value)).is_none(),
            "duplicate component tool name",
        )?;
    }
    need(
        map.contains_key(NAME) && map.contains_key("trustc"),
        "actual component tools incomplete",
    )?;
    Ok(V::Object(map))
}

fn doctor(report: &V, subject: &Path) -> R<()> {
    need(
        report["ready"].as_bool() == Some(true),
        "component doctor is not typed ready",
    )?;
    let c = &report["compiler"];
    need(
        c["trust_verify"].as_bool() == Some(true) && c["json_transport"].as_bool() == Some(true),
        "component doctor lacks verification/JSON capability",
    )?;
    let path = subject.join("bin/trustc").to_string_lossy().into_owned();
    need(
        c["path"].as_str() == Some(path.as_str())
            && c["linked_toolchain_path"].as_str() == Some(path.as_str())
            && c["discovery_source"].as_str() == Some("sibling_trustc")
            && c["linked_toolchain_status"].as_str() == Some("visible"),
        "component doctor selected another compiler",
    )?;
    let daily = &c["daily_driver"];
    need(
        daily["ready"].as_bool() == Some(true)
            && daily["linked_targo_path"].as_str()
                == Some(subject.join("bin/targo").to_string_lossy().as_ref())
            && daily["linked_targo_trust_path"].as_str()
                == Some(subject.join("bin/targo-trust").to_string_lossy().as_ref()),
        "component doctor selected another frontend",
    )
}

pub(super) fn validate(root: &Path, compiler: &str, expected: &str) -> R<Binding> {
    validate_selected(root, compiler, expected, None)
}

/// All historical frontend evidence remains strict. Only a separately validated
/// native component can project its two current tool hashes to the old map.
pub(super) fn validate_with_native(
    root: &Path,
    compiler: &str,
    expected: &str,
    native: &super::native_targo::Binding,
) -> R<Binding> {
    validate_selected(root, compiler, expected, Some(native))
}

fn validate_selected(
    root: &Path,
    compiler: &str,
    expected: &str,
    native: Option<&super::native_targo::Binding>,
) -> R<Binding> {
    need(
        aterm_codec::hex::is_lower(expected, 64),
        "invalid selected frontend component digest",
    )?;
    need(
        root.is_absolute()
            && root.canonicalize().is_ok_and(|p| p == root)
            && !std::fs::symlink_metadata(root)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink(),
        "component seal is not physical",
    )?;
    let details = root.join(DETAILS);
    need(
        details.canonicalize().is_ok_and(|p| p == details),
        "component evidence directory is not physical",
    )?;
    let record = document(&root.join("FRONTEND-COMPONENT.json"), Some(expected))?;
    shape(&record, compiler)?;
    fields(&record["identity_qualification"], "result source_review")?;
    reference(&record["identity_qualification"]["result"])?;
    reference(&record["identity_qualification"]["source_review"])?;
    let base = &record["base"];
    let receipt = document(
        &root.join("tool-provenance.json"),
        Some(reference(&base["accepted_stage_provenance"])?),
    )?;
    need(
        receipt["compiler"]["source_commit"].as_str() == Some(compiler)
            && receipt["host"] == base["host"],
        "original stage receipt source differs",
    )?;
    let payload = bytes(&root.join("bin/targo-trust"), 1 << 30)?;
    need(
        hash(&payload) == digest(&record["artifact"]["sha256"])?
            && record["artifact"]["bytes"].as_u64() == u64::try_from(payload.len()).ok(),
        "installed frontend bytes differ",
    )?;
    need(
        crate::toolchain::is_executable_file(&root.join("bin/targo-trust")),
        "installed frontend is not executable",
    )?;
    need(
        aterm_json::to_value(native_image(&payload, text(base, "host")?)?)
            .map_err(|e| e.to_string())?
            == record["artifact"]["system_libraries"],
        "frontend native libraries differ",
    )?;
    need(
        hash(&bytes(&root.join("bin/trustc"), 1 << 30)?) == digest(&base["compiler_sha256"])?,
        "installed compiler changed",
    )?;
    let built = copied(root, "build.json", reference(&record["build_receipt"])?)?;
    let controls = copied(
        root,
        "transport.json",
        reference(&record["transport_qualification"])?,
    )?;
    receipts(&record, &built, &controls)?;
    let identity = copied(
        root,
        "identity-witness.json",
        reference(&record["identity_qualification"]["result"])?,
    )?;
    need(
        identity["exit"].as_i64() == Some(0)
            && identity["source_unchanged"].as_bool() == Some(true)
            && identity["subject_unchanged"].as_bool() == Some(true)
            && identity["subject_artifact"] == record["artifact"],
        "identity witness is not successful for the same-source subject",
    )?;
    hashed_map(&controls["witness"]["source_files"], Some(&SOURCES))?;
    if controls["source_commit"] == record["source"]["commit"] {
        need(
            controls["source_relation"].is_null()
                && controls["witness"]["source_files"] == record["source"]["files"],
            "same-source transport witness differs",
        )?;
    } else {
        let relation = copied(
            root,
            "source-relation.json",
            reference(&controls["source_relation"])?,
        )?;
        need(
            relation["witness_source"] == controls["source_commit"]
                && relation["frontend_source"] == record["source"]["commit"]
                && relation["base"] == record["source"]["base"]
                && [
                    "test-only-and-documentation",
                    "reviewed-compile-time-oracle-binding",
                ]
                .contains(&text(&relation, "reviewed_change_kind")?),
            "source witness relation differs",
        )?;
        let shared = relation["shared_files"]
            .as_object()
            .ok_or("shared witness files absent")?;
        need(
            shared.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == SOURCES
                    .into_iter()
                    .filter(|s| s.starts_with("crates/trust-resource-controller/"))
                    .collect(),
            "shared witness readset differs",
        )?;
    }
    for stream_name in ["stdout", "stderr"] {
        stream(
            root,
            &format!("transport.{stream_name}"),
            &controls["phases"][0][stream_name],
        )?;
        let raw = stream(
            root,
            &format!("compiler-version.{stream_name}"),
            &built["compiler_version_phase"][stream_name],
        )?;
        if stream_name == "stdout" {
            version(&raw, text(&built["compiler"], "source")?, None, None)?;
        }
        let raw = stream(
            root,
            &format!("oracle-compiler-version.{stream_name}"),
            &built["retained_compiler_version_phase"][stream_name],
        )?;
        if stream_name == "stdout" {
            need(
                retained_release(&raw, compiler)? == text(&built["environment"], "CFG_RELEASE")?,
                "build release differs from retained compiler",
            )?;
        }
        stream(
            root,
            &format!("signature.{stream_name}"),
            &built["image_phase"][stream_name],
        )?;
        let raw = stream(
            root,
            &format!("version.{stream_name}"),
            &built["version_phase"][stream_name],
        )?;
        if stream_name == "stdout" {
            version(
                &raw,
                text(&record["source"], "commit")?,
                Some(compiler),
                Some(text(&built["environment"], "CFG_RELEASE")?),
            )?;
        }
    }
    let installation = document(&details.join("installation.json"), None)?;
    fields(
        &installation,
        "schema component_sha256 compiler_source before after source producer_validation_sha256 scope",
    )?;
    need(
        installation["schema"].as_str() == Some("trust.sdk-frontend-install.v2")
            && installation["component_sha256"].as_str() == Some(expected)
            && installation["compiler_source"].as_str() == Some(compiler)
            && installation["source"] == record["source"]["commit"],
        "frontend installation binding differs",
    )?;
    let validation = copied(
        root,
        "producer-validation.json",
        digest(&installation["producer_validation_sha256"])?,
    )?;
    need(
        validation == producer_validation(&record, &built, &controls, &identity)?,
        "producer validation attestation differs",
    )?;
    let before = installation["before"]
        .as_object()
        .ok_or("installation before map absent")?;
    let after = installation["after"]
        .as_object()
        .ok_or("installation after map absent")?;
    let mut expected_after = before.clone();
    expected_after.insert(NAME.to_owned(), record["artifact"]["sha256"].clone());
    need(
        before.get(NAME) == Some(&base["original_targo_trust_sha256"]) && after == &expected_after,
        "installation changed another tool",
    )?;
    for (name, value) in base["existing_tool_sha256"]
        .as_object()
        .ok_or("original map absent")?
    {
        need(
            before.get(name) == Some(value),
            "installation original tool map differs",
        )?;
    }
    for value in before.values().chain(after.values()) {
        digest(value)?;
    }
    let actual = tool_map(root)?;
    let actual = actual.as_object().ok_or("actual tool map absent")?;
    let projected = native
        .map(|binding| binding.project_frontend_tools(root, compiler, expected, actual))
        .transpose()?;
    let actual = projected.as_ref().unwrap_or(actual);
    need(
        after
            .iter()
            .all(|(name, value)| actual.get(name) == Some(value))
            && actual
                .keys()
                .all(|name| after.contains_key(name) || WP.contains(&name.as_str())),
        "installed tools changed or unexpected tool added",
    )?;
    qualified(root, compiler, expected, &record, &installation)?;
    Ok(Binding {
        original: digest(&base["original_targo_trust_sha256"])?.to_owned(),
        replacement: digest(&record["artifact"]["sha256"])?.to_owned(),
        mint: reference(&base["original_mint"])?.to_owned(),
    })
}

fn qualified(root: &Path, compiler: &str, expected: &str, record: &V, installation: &V) -> R<()> {
    let value = document(&root.join("FRONTEND-QUALIFIED.json"), None)?;
    fields(
        &value,
        "schema status component_sha256 source compiler_source host subject tool_sha256 doctor_phase scope",
    )?;
    need(
        value["schema"].as_str() == Some("trust.sdk-frontend-qualified.v2")
            && value["status"].as_str() == Some("qualified")
            && value["component_sha256"].as_str() == Some(expected)
            && value["source"] == record["source"]["commit"]
            && value["compiler_source"].as_str() == Some(compiler)
            && value["host"] == record["base"]["host"]
            && value["tool_sha256"] == installation["after"],
        "frontend is not qualified for selected subject",
    )?;
    // Copies retain the original actual doctor's subject. Evidence is read by
    // fixed local names; recorded historical absolute paths grant no file IO.
    let subject = absolute(&value["subject"])?;
    let phase = &value["doctor_phase"];
    fields(phase, "name exit stdout stderr context")?;
    need(
        phase["name"].as_str() == Some("doctor-report") && phase["exit"].as_i64() == Some(0),
        "doctor phase is not typed actual0",
    )?;
    let mut stdout = None;
    for name in ["stdout", "stderr"] {
        let expected_path = subject.join(DETAILS).join(format!("doctor.{name}"));
        need(
            absolute(&phase[name]["path"])? == expected_path,
            "doctor raw subject differs",
        )?;
        let raw = stream(root, &format!("doctor.{name}"), &phase[name])?;
        if name == "stdout" {
            stdout = Some(raw);
        }
    }
    let context_ref = &phase["context"];
    need(
        absolute(&context_ref["path"])? == subject.join(DETAILS).join("doctor.context.json"),
        "doctor context subject differs",
    )?;
    let context = copied(root, "doctor.context.json", reference(context_ref)?)?;
    fields(
        &context,
        "actual_exit argv cargo_home cache_preparation initially_empty",
    )?;
    need(
        context["actual_exit"].as_i64() == Some(0)
            && context["initially_empty"].as_bool() == Some(true)
            && context["argv"]
                == aterm_json::json!([
                    subject.join("bin/targo").to_string_lossy().into_owned(),
                    "trust",
                    "doctor",
                    "--format",
                    "json"
                ]),
        "doctor execution context differs",
    )?;
    let Unique(report) = aterm_json::from_slice::<Unique>(&stdout.ok_or("doctor stdout absent")?)
        .map_err(|e| e.to_string())?;
    need(report.is_object(), "doctor report is not object")?;
    doctor(&report, subject)
}

#[cfg(test)]
mod tests;
