// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inert portable receipts and synthetic Mach-O bytes; no compiler/tool executes.

use super::*;
use aterm_json::json;
use std::path::PathBuf;

const COMPILER: &str = "1111111111111111111111111111111111111111";
const REPAIR: &str = "2222222222222222222222222222222222222222";

fn edit<'a>(mut value: &'a mut V, path: &[&str]) -> &'a mut V {
    for key in path {
        value = match value {
            V::Object(map) => map.entry((*key).to_owned()).or_insert(V::Null),
            V::Array(rows) => rows.get_mut(key.parse::<usize>().unwrap()).unwrap(),
            _ => panic!("declared fixture container"),
        };
    }
    value
}

fn save(path: &Path, value: &V) -> V {
    let raw = aterm_json::to_vec(value).unwrap();
    std::fs::write(path, &raw).unwrap();
    json!({"path":path.to_string_lossy().into_owned(), "sha256":hash(&raw)})
}

fn raw(root: &Path, name: &str, content: &[u8]) -> V {
    let path = root.join(DETAILS).join(name);
    std::fs::write(&path, content).unwrap();
    json!({"path":path.to_string_lossy().into_owned(), "sha256":hash(content)})
}

fn observation(root: &Path, name: &str, local: &str) -> V {
    json!({"name":name, "exit":0,
        "stdout":raw(root, &format!("{local}.stdout"), b"inert observation"),
        "stderr":raw(root, &format!("{local}.stderr"), b"")})
}

fn image() -> Vec<u8> {
    let library = b"/usr/lib/libSystem.B.dylib\0";
    let length = (24 + library.len()).next_multiple_of(8);
    let mut raw = Vec::new();
    for word in [
        0xfeed_facf,
        0x0100_000c,
        0,
        2,
        1,
        u32::try_from(length).unwrap(),
        0,
        0,
    ] {
        raw.extend_from_slice(&word.to_le_bytes());
    }
    for word in [0xc, u32::try_from(length).unwrap(), 24, 0, 0, 0] {
        raw.extend_from_slice(&word.to_le_bytes());
    }
    raw.extend_from_slice(library);
    raw.resize(32 + length, 0);
    raw
}

struct Fixture {
    _dir: crate::testdir::Dir,
    root: PathBuf,
    record: V,
    built: V,
    controls: V,
    identity: V,
    before: V,
    expected: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = crate::testdir::dir("frontend-portable-inert");
        let root = dir.canonicalize().unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join(DETAILS)).unwrap();
        for name in ["trustc", "targo", NAME] {
            std::fs::write(
                root.join("bin").join(name),
                format!("inert original {name}"),
            )
            .unwrap();
        }
        let before = tool_map(&root).unwrap();
        let tree = "a".repeat(64);
        let mint = format!(
            "schema=trust-seed-mint-v1\nsource_commit={COMPILER}\nddc_seed_relation=pin-ancestor\nddc_verdict=identical\nstage2_tree_sha256_pinned_seed={tree}\nstage2_tree_sha256_ddc_seed={tree}\nrepin_commit={REPAIR}\nrepin_compiler_git_commit_hash={COMPILER}\ncompiler_file={} bin/targo-trust\ncompiler_file={} bin/trustc\ncompiler_file={} bin/targo\n",
            before[NAME].as_str().unwrap(),
            before["trustc"].as_str().unwrap(),
            before["targo"].as_str().unwrap()
        );
        std::fs::write(root.join("SEED-MINT"), mint.as_bytes()).unwrap();
        let accepted = save(
            &root.join("tool-provenance.json"),
            &json!({"compiler":{"source_commit":COMPILER},"host":"aarch64-apple-darwin"}),
        );
        let payload = image();
        std::fs::write(root.join("bin").join(NAME), &payload).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join("bin").join(NAME),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let compiler = json!({"path":"/inert/build/bin/trustc","sha256":hash(b"inert compiler"),"source":COMPILER,"sysroot":"/inert/build"});
        let files = json!({"crates/trust-resource-controller/src/client.rs":hash(b"inert source")});
        let mut built = json!({
            "source_commit":REPAIR,"compiler":compiler,"target":"aarch64-apple-darwin",
            "argv":["/inert/build/bin/targo","--unverified","build","--release","--locked","--offline","--manifest-path","/inert/source/targo-trust/Cargo.toml","--features",FEATURES,"--bin",NAME],
            "environment":{"CFG_VER_HASH":REPAIR,"CFG_RELEASE":"1.101.0-dev","CFG_TRUST_ORACLE_HOST":"1","CFG_TRUST_ORACLE_COMPILER_HASH":COMPILER,"CLEAN_OLEAN_REQUIRE_PINNED_LEAN":"1","ELAN_HOME":"/inert/elan"},
            "context":{"path":"/inert/context","sha256":hash(b"inert context")},
            "compiler_version_phase":observation(&root,"build-compiler-version","compiler-version"),
            "retained_compiler_version_phase":observation(&root,"retained-compiler-version","oracle-compiler-version"),
            "version_phase":observation(&root,"frontend-version","version"),
            "image_phase":observation(&root,"artifact-code-signature","signature"),
            "generated_inputs":{"scope":"inert producer-attested data, never opened"},
            "target_directory":"/inert/target","lock_sha256":hash(b"inert lock"),
            "artifact_sha256":hash(&payload),"retained_inputs_unchanged":true,
            "phases":[observation(&root,"frontend-build","build-unused")]
        });
        *edit(&mut built, &["compiler_version_phase", "argv"]) =
            json!(["/inert/build/bin/trustc", "-vV"]);
        *edit(&mut built, &["compiler_version_phase", "stdout"]) = raw(
            &root,
            "compiler-version.stdout",
            format!("rustc compatibility report\nbinary: trustc\ncommit-hash: {COMPILER}\n")
                .as_bytes(),
        );
        *edit(&mut built, &["retained_compiler_version_phase", "argv"]) =
            json!(["/inert/original-stage/bin/trustc", "-vV"]);
        *edit(&mut built, &["retained_compiler_version_phase", "stdout"]) = raw(
            &root,
            "oracle-compiler-version.stdout",
            format!("binary: trustc\ncommit-hash: {COMPILER}\nrelease: 1.101.0-dev\n").as_bytes(),
        );
        *edit(&mut built, &["image_phase", "argv"]) = json!([
            "/usr/bin/codesign",
            "--verify",
            "--strict",
            "/inert/artifact"
        ]);
        *edit(&mut built, &["version_phase", "stdout"]) = raw(&root,"version.stdout",format!("targo-trust 0.13.0\ntrust-repo-commit-hash: {REPAIR}\ntrust-oracle-compiler-commit-hash: {COMPILER}\ntrust-oracle-compiler-release: 1.101.0-dev\n").as_bytes());
        let controls = json!({"source_commit":REPAIR,"source_tree":REPAIR,"source_relation":null,
            "witness":{"path":"/inert/witness","sha256":hash(b"inert witness"),"compiler":compiler,"features":["probe-scheduler"],"source_files":files,"target":"aarch64-apple-darwin","argv":["inert-targo","--locked","--offline"],"lock_sha256":hash(b"inert witness lock")},
            "controls":CONTROLS,"phases":[observation(&root,"probe-transport-controls","transport")]});
        let mut identity = json!({"exit":0,"source_unchanged":true,"subject_unchanged":true,"subject_artifact":null,"harness":{"sha256":hash(b"inert same-source harness")}});
        let record = json!({"schema":"trust.sdk-frontend-component.v2","status":"prepared","name":NAME,
            "base":{"compiler_source":COMPILER,"host":"aarch64-apple-darwin","compiler_sha256":before["trustc"],"original_stage":"/inert/original-stage","original_mint":{"path":"/inert/trust-seed-mint","sha256":hash(mint.as_bytes())},"accepted_stage_provenance":accepted,"original_targo_trust_sha256":before[NAME],"existing_tool_sha256":before},
            "source":{"root":"/inert/source","commit":REPAIR,"base":COMPILER,"tree":REPAIR,"files":files},
            "build_receipt":null,"artifact":{"path":"/inert/artifact","sha256":hash(&payload),"bytes":payload.len(),"system_libraries":["/usr/lib/libSystem.B.dylib"]},
            "transport_qualification":null,"identity_qualification":{"result":null,"source_review":{"path":"/inert/review","sha256":hash(b"inert review")}},"retained_inputs_unchanged":true});
        *edit(&mut identity, &["subject_artifact"]) = record["artifact"].clone();
        let mut fixture = Self {
            _dir: dir,
            root,
            record,
            built,
            controls,
            identity,
            before,
            expected: String::new(),
        };
        fixture.refresh();
        fixture
    }

    fn refresh(&mut self) {
        let detail = self.root.join(DETAILS);
        *edit(&mut self.record, &["build_receipt"]) = save(&detail.join("build.json"), &self.built);
        *edit(&mut self.record, &["transport_qualification"]) =
            save(&detail.join("transport.json"), &self.controls);
        *edit(&mut self.record, &["identity_qualification", "result"]) =
            save(&detail.join("identity-witness.json"), &self.identity);
        self.expected = save(&self.root.join("FRONTEND-COMPONENT.json"), &self.record)["sha256"]
            .as_str()
            .unwrap()
            .to_owned();
        let validation =
            producer_validation(&self.record, &self.built, &self.controls, &self.identity).unwrap();
        let validation = save(&detail.join("producer-validation.json"), &validation);
        let mut after = self.before.clone();
        *edit(&mut after, &[NAME]) = self.record["artifact"]["sha256"].clone();
        save(
            &detail.join("installation.json"),
            &json!({"schema":"trust.sdk-frontend-install.v2","component_sha256":self.expected,"compiler_source":COMPILER,"before":self.before,"after":after,"source":REPAIR,"producer_validation_sha256":validation["sha256"],"scope":"inert fixture"}),
        );
        // Historical subject need not exist: portable readers use only fixed copied names.
        let subject = Path::new("/inert/historical-seal");
        let report = json!({"ready":true,"compiler":{"path":subject.join("bin/trustc").to_string_lossy().into_owned(),"linked_toolchain_path":subject.join("bin/trustc").to_string_lossy().into_owned(),"discovery_source":"sibling_trustc","linked_toolchain_status":"visible","trust_verify":true,"json_transport":true,"daily_driver":{"ready":true,"linked_targo_path":subject.join("bin/targo").to_string_lossy().into_owned(),"linked_targo_trust_path":subject.join("bin/targo-trust").to_string_lossy().into_owned()}}});
        let mut stdout = raw(
            &self.root,
            "doctor.stdout",
            &aterm_json::to_vec(&report).unwrap(),
        );
        let mut stderr = raw(&self.root, "doctor.stderr", b"");
        let mut context = save(
            &detail.join("doctor.context.json"),
            &json!({"actual_exit":0,"argv":[subject.join("bin/targo").to_string_lossy().into_owned(),"trust","doctor","--format","json"],"cargo_home":"/inert/empty-home","cache_preparation":"inert fixture","initially_empty":true}),
        );
        for (reference, local) in [
            (&mut stdout, "doctor.stdout"),
            (&mut stderr, "doctor.stderr"),
            (&mut context, "doctor.context.json"),
        ] {
            *edit(reference, &["path"]) = V::from(
                subject
                    .join(DETAILS)
                    .join(local)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        save(
            &self.root.join("FRONTEND-QUALIFIED.json"),
            &json!({"schema":"trust.sdk-frontend-qualified.v2","status":"qualified","component_sha256":self.expected,"source":REPAIR,"compiler_source":COMPILER,"host":"aarch64-apple-darwin","subject":subject.to_string_lossy().into_owned(),"tool_sha256":after,"doctor_phase":{"name":"doctor-report","exit":0,"stdout":stdout,"stderr":stderr,"context":context},"scope":"inert fixture; no actual product grade"}),
        );
    }

    fn check(&self) -> R<Binding> {
        validate(&self.root, COMPILER, &self.expected)
    }
}

#[test]
fn qualified_portable_copy_allows_only_explicit_frontend_comparison() {
    let f = Fixture::new();
    f.check().unwrap_or_else(|why| panic!("{why}"));
    assert!(!super::super::seed_mint_refusals(&f.root, COMPILER).is_empty());
    assert!(
        super::super::seed_mint_refusals_with_frontend(&f.root, COMPILER, Some(&f.expected))
            .is_empty()
    );
    std::fs::write(f.root.join("bin/targo"), b"changed other original tool").unwrap();
    assert!(f.check().is_err());
}

#[test]
fn coherent_missing_duplicate_link_or_wrong_original_mint_target_refuses() {
    for kind in ["missing", "duplicate", "link", "digest", "mint"] {
        let mut f = Fixture::new();
        let path = f.root.join("SEED-MINT");
        let original = std::fs::read_to_string(&path).unwrap();
        let row = original
            .lines()
            .find(|s| s.ends_with(" bin/targo-trust"))
            .unwrap();
        let changed = match kind {
            "missing" => original.replace(&format!("{row}\n"), ""),
            "duplicate" => format!("{original}{row}\n"),
            "link" => original.replace(row, "compiler_link=elsewhere bin/targo-trust"),
            "digest" => original.replace(
                row,
                &format!("compiler_file={} bin/targo-trust", "f".repeat(64)),
            ),
            _ => format!("{original}# distinct receipt\n"),
        };
        std::fs::write(&path, &changed).unwrap();
        if kind != "mint" {
            *edit(&mut f.record, &["base", "original_mint", "sha256"]) =
                V::from(hash(changed.as_bytes()));
            f.refresh();
        }
        assert!(
            !super::super::seed_mint_refusals_with_frontend(&f.root, COMPILER, Some(&f.expected))
                .is_empty(),
            "{kind}"
        );
    }
}

#[test]
fn explicit_receipt_hash_prepared_only_and_typed_doctor_status_refuse() {
    let f = Fixture::new();
    assert!(validate(&f.root, COMPILER, &"f".repeat(64)).is_err());
    let path = f.root.join("FRONTEND-QUALIFIED.json");
    let mut value = document(&path, None).unwrap();
    *edit(&mut value, &["doctor_phase", "exit"]) = V::from(false);
    save(&path, &value);
    assert!(f.check().is_err());
    std::fs::remove_file(path).unwrap();
    assert!(f.check().is_err());
}

#[test]
fn native_image_and_copied_stream_mutations_refuse() {
    let f = Fixture::new();
    std::fs::write(f.root.join(DETAILS).join("transport.stdout"), b"changed").unwrap();
    assert!(f.check().is_err());
    let mut payload = image();
    payload[4] = 7;
    assert!(native_image(&payload, "aarch64-apple-darwin").is_err());
    let mut payload = image();
    payload[56] = b'x';
    assert!(native_image(&payload, "aarch64-apple-darwin").is_err());
}

#[test]
fn json_duplicate_keys_and_noninteger_phase_status_refuse() {
    assert!(aterm_json::from_slice::<Unique>(br#"{"x":{"exit":0,"exit":1}}"#).is_err());
    let mut f = Fixture::new();
    *edit(&mut f.built, &["phases", "0", "exit"]) = V::from(false);
    f.refresh();
    assert!(f.check().is_err());
}

#[test]
fn compiler_observation_requires_the_exact_binary_line() {
    for line in ["", "binary: rustc", "binary: trustc extra"] {
        let mut f = Fixture::new();
        *edit(&mut f.built, &["compiler_version_phase", "stdout"]) = raw(
            &f.root,
            "compiler-version.stdout",
            format!("trustc compatibility report\n{line}\ncommit-hash: {COMPILER}\n").as_bytes(),
        );
        f.refresh();
        assert!(f.check().is_err(), "{line}");
    }
}

#[test]
fn v2_rejects_v1_and_unbound_or_duplicate_retained_release() {
    let mut f = Fixture::new();
    *edit(&mut f.record, &["schema"]) = V::from("trust.sdk-frontend-component.v1");
    f.refresh();
    assert!(f.check().is_err());
    for tail in [
        "",
        "release: 0.13.0\n",
        "release:X1.101.0-dev\n",
        "release: 1.101.0-dev\nrelease: 1.101.0-dev\n",
    ] {
        let mut f = Fixture::new();
        *edit(&mut f.built, &["retained_compiler_version_phase", "stdout"]) = raw(
            &f.root,
            "oracle-compiler-version.stdout",
            format!("binary: trustc\ncommit-hash: {COMPILER}\n{tail}").as_bytes(),
        );
        f.refresh();
        assert!(f.check().is_err(), "{tail}");
    }
}

#[cfg(unix)]
#[test]
fn selected_symlink_payload_or_evidence_and_unexpected_tools_refuse() {
    use std::os::unix::fs::symlink;
    for name in [
        "bin/targo-trust",
        "FRONTEND-COMPONENT.evidence/transport.stdout",
    ] {
        let f = Fixture::new();
        let path = f.root.join(name);
        let moved = path.with_extension("original");
        std::fs::rename(&path, &moved).unwrap();
        symlink(&moved, &path).unwrap();
        assert!(f.check().is_err());
    }
    let f = Fixture::new();
    std::fs::write(f.root.join("bin/unexpected"), b"extra").unwrap();
    assert!(f.check().is_err());
    std::fs::remove_file(f.root.join("bin/unexpected")).unwrap();
    std::fs::write(f.root.join("bin/trust-wp"), b"allowed supplementary WP").unwrap();
    assert!(f.check().is_ok());
}
