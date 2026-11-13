// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn source_capabilities_are_bounded_without_package_declarations() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let source = dir.path().join("lib.rs");
    std::fs::write(
        &source,
        "pub fn value() -> u8 { 1 } // include_str!(ignored)\n",
    )
    .unwrap();
    assert!(source_readers_bounded(dir.path()).is_ok());
    for text in [
        "const S: &str = include_str!(\"outside\");",
        "macro_rules! reader { () => { std::env!(\"HOME\") } }",
        "#[cfg(any())] const S: &str = option_env!(\"HOME\");",
        "#[path = \"../../outside.rs\"] mod redirected;",
        "core::arch::global_asm!(\".incbin outside\");",
        "concat_idents!(en, v);",
    ] {
        std::fs::write(&source, text).unwrap();
        assert!(source_readers_bounded(dir.path()).is_err(), "{text}");
    }
}

#[test]
fn opaque_compiler_config_cannot_seed_a_verdict() {
    for text in [
        "include = ['outside.toml']",
        "[build]\ntarget='outside.json'",
        "[env]\nRUST_TARGET_PATH='/outside'",
        "[build]\nrustflags=['--sysroot=/outside']",
        "[build]\nrustflags=['@outside.flags']",
        "[build]\nrustflags=['--extern=unknown=/outside/lib.rlib']",
        "[env]\nSYSROOT='/outside'",
        "[env]\nDYLD_LIBRARY_PATH='/outside'",
        "[unstable]\nbuild-std=['std']",
        "[profile.dev]\ncodegen-backend='/outside'",
    ] {
        let config = aterm_toml::from_str(text).unwrap();
        assert!(bounded_config_readers(&config).is_err(), "{text}");
    }
    let config =
        aterm_toml::from_str("[target.'cfg(trust_verify)']\nrustflags=['-Zthreads=4']").unwrap();
    assert!(bounded_config_readers(&config).is_ok());
}

fn evidence(byte: &str) -> Evidence {
    Evidence {
        content_key: Some(byte.repeat(64)),
        reason: None,
    }
}

#[test]
fn proof_parser_refuses_fail_unbounded_claims_and_duplicate_specs() {
    let proof = CarryProof::passed("a", &evidence("a"), 1.0);
    let passed = Passed::from([("a".into(), proof.clone())]);
    let encoded = encode_passed(&passed).unwrap();
    assert_eq!(read_passed(&encoded).unwrap(), passed);

    let mut fail = proof.clone();
    fail.outcome = "FAIL".into();
    assert!(read_passed(&aterm_json::to_string(&fail).unwrap()).is_err());
    let duplicate = format!("{encoded}{encoded}");
    assert!(read_passed(&duplicate).unwrap_err().contains("repeats"));

    let mut contradiction = proof;
    contradiction.content_key = None;
    assert!(
        read_passed(&aterm_json::to_string(&contradiction).unwrap())
            .unwrap_err()
            .contains("bounded")
    );
}

#[cfg(unix)]
#[test]
fn compiler_tool_key_changes_when_equal_bytes_execution_metadata_changes() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = aterm_tempfile::tempdir().unwrap();
    let tool = dir.path().join("tippy");
    std::fs::write(&tool, b"same compiler tool bytes").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

    let key = |path: &Path| {
        let mut hash = aterm_digest::Sha256::new();
        hash_compiler_tool(&mut hash, path).unwrap();
        aterm_codec::hex::encode_lower(&hash.finalize())
    };
    let before_digest = digest_file(&tool).unwrap();
    let before_key = key(&tool);
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(before_digest, digest_file(&tool).unwrap());
    assert_ne!(before_key, key(&tool));
}

#[test]
fn cargo_env_wrapper_and_clippy_config_bytes_are_in_the_key() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("wrapper");
    let clippy = dir.path().join("clippy");
    std::fs::create_dir_all(&clippy).unwrap();
    std::fs::write(&wrapper, b"wrapper one").unwrap();
    std::fs::write(clippy.join("clippy.toml"), b"msrv = '1.80'").unwrap();
    let allowed = BTreeSet::from([wrapper.canonicalize().unwrap()]);
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[env]\nRUSTC_WRAPPER = {{ value = '{}' }}\nCLIPPY_CONF_DIR = '{}'\n",
            wrapper.display(),
            clippy.display()
        ),
    )
    .unwrap();
    let key = || {
        let mut hash = aterm_digest::Sha256::new();
        let mut clippy_dirs = BTreeSet::new();
        hash_cargo_config(&mut hash, &config, &allowed, &mut clippy_dirs).unwrap();
        for directory in clippy_dirs {
            hash_clippy_directory(&mut hash, &directory).unwrap();
        }
        aterm_codec::hex::encode_lower(&hash.finalize())
    };

    let first = key();
    std::fs::write(&wrapper, b"wrapper two").unwrap();
    let wrapper_changed = key();
    assert_ne!(first, wrapper_changed);
    std::fs::write(clippy.join("clippy.toml"), b"msrv = '1.81'").unwrap();
    assert_ne!(wrapper_changed, key());

    let undeclared = dir.path().join("undeclared-wrapper");
    std::fs::write(&undeclared, b"unknown").unwrap();
    let mut hash = aterm_digest::Sha256::new();
    assert!(
        hash_selected_tool(
            &mut hash,
            "RUSTC_WRAPPER",
            &os_bytes(undeclared.as_os_str()),
            &allowed,
        )
        .unwrap_err()
        .contains("undeclared")
    );
}

#[test]
fn one_lint_package_cannot_mix_tippy_programs() {
    let commands = [Cmd::new("/fixture/tippy-a"), Cmd::new("/fixture/tippy-b")];
    assert!(shared_command_program(&commands).is_err());
    assert_eq!(
        shared_command_program(&commands[..1]).unwrap(),
        Path::new("/fixture/tippy-a")
    );
}

#[cfg(unix)]
#[test]
fn verifier_jobserver_path_is_not_lint_content_but_width_and_unknown_flags_are() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let context = |name: &str| {
        let root = dir.path().join(format!("{name}-root"));
        std::fs::create_dir_all(&root).unwrap();
        let mut ctx = crate::Ctx::new(
            root.clone(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            root.join("scratch"),
        );
        ctx.pool = Some(
            aterm_capacity::Jobserver::new(&dir.path().join(format!("{name}.jobs")), 8)
                .unwrap()
                .into(),
        );
        ctx
    };
    let first_ctx = context("first");
    let next_ctx = context("next");
    let command = |flags: &str, jobs: &str| {
        let mut command = Cmd::new("/fixture/tippy");
        command.cwd = Some(PathBuf::from("/fixture/workspace"));
        command.exact_env = true;
        command.envs = vec![
            ("MAKEFLAGS".into(), flags.into()),
            ("CARGO_MAKEFLAGS".into(), flags.into()),
            ("CARGO_BUILD_JOBS".into(), jobs.into()),
        ];
        command
    };
    let identity = |ctx: &crate::Ctx, command: &Cmd, jobs: Option<u32>| -> Result<String, String> {
        crate::compile_width::with_jobs(jobs, || {
            let mut hash = aterm_digest::Sha256::new();
            hash_command_identity(&mut hash, ctx, command)?;
            Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
        })
    };

    let first_flags = first_ctx.pool.as_deref().unwrap().flags();
    let next_flags = next_ctx.pool.as_deref().unwrap().flags();
    let first = command(&first_flags, "8");
    let next = command(&next_flags, "8");
    assert_eq!(
        identity(&first_ctx, &first, Some(8)).unwrap(),
        identity(&next_ctx, &next, Some(8)).unwrap(),
        "only the verifier-owned FIFO capability path changes between release processes"
    );

    let narrower = command(&next_flags, "7");
    assert_ne!(
        identity(&next_ctx, &next, Some(8)).unwrap(),
        identity(&next_ctx, &narrower, Some(8)).unwrap(),
        "the effective compiler width remains an exact input"
    );

    let unknown = format!("{next_flags} --shuffle");
    assert!(
        identity(&next_ctx, &command(&unknown, "8"), Some(8))
            .unwrap_err()
            .contains("active verifier jobserver flags"),
        "an unknown active-pool flag makes the package unbounded"
    );
    for index in [0, 1] {
        let mut unpaired = command(&next_flags, "8");
        unpaired.envs[index].1 = first_flags.clone().into();
        assert!(
            identity(&next_ctx, &unpaired, Some(8)).is_err(),
            "each GNU flag variable must exactly name the active verifier pool"
        );
    }
    for missing in ["MAKEFLAGS", "CARGO_MAKEFLAGS"] {
        let mut unpaired = command(&next_flags, "8");
        unpaired.envs.retain(|(name, _)| name != missing);
        assert!(
            identity(&next_ctx, &unpaired, Some(8)).is_err(),
            "both GNU flag variables are required: missing {missing}"
        );
    }
    let mut missing_width = command(&next_flags, "8");
    missing_width
        .envs
        .retain(|(name, _)| name != "CARGO_BUILD_JOBS");
    assert!(
        identity(&next_ctx, &missing_width, Some(8)).is_err(),
        "an active verifier command without its effective width is unbounded"
    );
    for invalid in ["0", "08", "nine", "9"] {
        assert!(
            identity(&next_ctx, &command(&next_flags, invalid), Some(8)).is_err(),
            "an invalid, noncanonical or over-grant width is unbounded: {invalid}"
        );
    }

    let mut unpooled_ctx = context("unpooled");
    unpooled_ctx.pool = None;
    assert!(
        identity(&unpooled_ctx, &command(&first_flags, "8"), Some(8))
            .unwrap_err()
            .contains("no adopted jobserver"),
        "compiler-width policy cannot normalize a jobserver the verifier does not own"
    );
    assert_ne!(
        identity(&unpooled_ctx, &command(&first_flags, "8"), None).unwrap(),
        identity(&unpooled_ctx, &command(&next_flags, "8"), None).unwrap(),
        "a user-owned jobserver path remains exact outside the active verifier pool"
    );
}

/// The production evidence path, not merely its normalizer, must ignore only the active
/// verifier FIFO's invocation-local pathname. NEGATIVE CONTROLS: reverting the
/// [`EvidenceBuilder::content_key`] call to raw [`command_identity`] makes the first equality
/// fail; normalizing an adopted but inactive pool makes the last inequality fail.
#[cfg(unix)]
#[test]
fn evidence_builder_normalizes_only_its_active_verifier_jobserver() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().join("workspace");
    let package_root = root.join("crates/fixture");
    let stage2 = root.join("tools/bin");
    let cargo_home = root.join("cargo-home");
    let scratch = root.join("scratch");
    for path in [&package_root, &stage2, &cargo_home, &scratch] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::create_dir_all(package_root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = ['crates/fixture']\nresolver = '2'\n",
    )
    .unwrap();
    std::fs::write(
        package_root.join("Cargo.toml"),
        "[package]\nname = 'fixture'\nversion = '0.0.0'\nedition = '2024'\n",
    )
    .unwrap();
    std::fs::write(package_root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();

    let tool = |name: &str| {
        let path = stage2.join(crate::toolchain::tool_file(name));
        std::fs::write(&path, format!("fixture {name}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let targo = tool("targo");
    let trustdoc = tool("trustdoc");
    let tippy = tool("targo-tippy");
    let _driver = tool("tippy-driver");
    let _trustc = tool("trustc");
    let _trustd = tool("trustd");
    let tools = crate::Toolchain {
        stage2_dir: stage2,
        targo,
        trustdoc,
        tippy: Some(tippy.clone()),
        trustfmt: None,
        borrowed: Vec::new(),
        refused: None,
        store_bin: None,
        demoted: None,
        lock: None,
        lock_refusal: None,
        lock_override: None,
    };
    let context = |name: &str| {
        let mut ctx = crate::Ctx::new_with_tools(
            root.clone(),
            crate::Scope::workspace(),
            crate::EnvSnapshot {
                home: root.join("home"),
                ..crate::EnvSnapshot::default()
            },
            scratch.clone(),
            tools.clone(),
        );
        ctx.carry_context = Some("11".repeat(32));
        ctx.pool = Some(
            aterm_capacity::Jobserver::new(&root.join(format!("{name}.jobs")), 8)
                .unwrap()
                .into(),
        );
        ctx
    };
    let first_ctx = context("first");
    let next_ctx = context("next");

    let id = "fixture 0.0.0".to_owned();
    let manifest = package_root.join("Cargo.toml").canonicalize().unwrap();
    let package_root = package_root.canonicalize().unwrap();
    let graph = CargoGraph {
        digest: "22".repeat(32),
        metadata: None,
        workspace: BTreeSet::from([id.clone()]),
        packages: BTreeMap::from([(
            id.clone(),
            MetadataPackage {
                id: id.clone(),
                manifest_path: manifest.clone(),
                targets: vec![MetadataTarget {
                    kind: vec!["lib".into()],
                    src_path: Some(package_root.join("src/lib.rs")),
                }],
                links: None,
            },
        )]),
        nodes: BTreeMap::from([(id.clone(), Vec::new())]),
        package_by_root: BTreeMap::from([(package_root, id)]),
    };
    let command = |ctx: &crate::Ctx, flags: &str, jobs: &str| {
        let argv = [tippy.clone().into_os_string()];
        let recorded = Cmd::replayed(
            &argv,
            &root,
            vec![
                ("CARGO_HOME".into(), cargo_home.clone().into_os_string()),
                ("MAKEFLAGS".into(), flags.into()),
                ("CARGO_MAKEFLAGS".into(), flags.into()),
                ("CARGO_BUILD_JOBS".into(), jobs.into()),
            ],
        )
        .counting_waits();
        crate::compile_width::with_jobs(Some(8), || {
            crate::exec::finalize_exact_command(recorded, ctx.exec_env()).unwrap()
        })
    };
    let evidence = |ctx: &crate::Ctx, command: &Cmd, jobs: Option<u32>| {
        crate::compile_width::with_jobs(jobs, || {
            EvidenceBuilder::new(ctx, &graph).evidence(
                "fixture",
                "crates/fixture",
                std::slice::from_ref(command),
            )
        })
    };

    let first_flags = first_ctx.pool.as_deref().unwrap().flags();
    let next_flags = next_ctx.pool.as_deref().unwrap().flags();
    let first = command(&first_ctx, &first_flags, "8");
    let next = command(&next_ctx, &next_flags, "8");
    let first_active = evidence(&first_ctx, &first, Some(8));
    let next_active = evidence(&next_ctx, &next, Some(8));
    assert!(first_active.bounded(), "{first_active:?}");
    assert_eq!(
        first_active, next_active,
        "the production content key ignores only the active verifier FIFO pathname"
    );

    let unknown = command(&next_ctx, &format!("{next_flags} --shuffle"), "8");
    let unknown = evidence(&next_ctx, &unknown, Some(8));
    assert!(
        !unknown.bounded()
            && unknown
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("active verifier jobserver flags")),
        "an unknown verifier flag must fail the production proof closed: {unknown:?}"
    );

    let mut unowned_ctx = context("unowned");
    unowned_ctx.pool = None;
    let unowned = evidence(&unowned_ctx, &first, Some(8));
    assert!(
        !unowned.bounded()
            && unowned
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("no adopted jobserver")),
        "the production proof cannot normalize a jobserver it does not own: {unowned:?}"
    );

    assert_ne!(
        evidence(&first_ctx, &first, None),
        evidence(&next_ctx, &next, None),
        "an adopted pool's FIFO remains exact when compiler-width policy is inactive"
    );
}

#[test]
fn boundary_rejects_a_command_program_other_than_its_captured_tippy() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let captured = dir.path().join("captured-tippy");
    let substituted = dir.path().join("substituted-tippy");
    std::fs::write(&captured, b"captured").unwrap();
    std::fs::write(&substituted, b"substituted").unwrap();
    let physical = captured.canonicalize().unwrap();
    assert!(boundary_program_matches(&captured, &physical, &[Cmd::new(&captured)]).is_ok());
    assert!(
        boundary_program_matches(&captured, &physical, &[Cmd::new(&substituted)])
            .unwrap_err()
            .contains("differs"),
        "a prepared command cannot borrow the constructor's compiler digest"
    );
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias/tippy");
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&captured, &alias).unwrap();
        assert!(
            boundary_program_matches(&captured, &physical, &[Cmd::new(alias)]).is_err(),
            "an argv0 alias may select a different sibling driver and is not interchangeable"
        );
    }
}

#[test]
fn exact_rows_partition_and_an_unbounded_reader_runs() {
    let exact = Package {
        spec: "exact".into(),
        dir: "crates/exact".into(),
        evidence: evidence("a"),
        commands: Vec::new(),
    };
    let changed = Package {
        spec: "changed".into(),
        dir: "crates/changed".into(),
        evidence: evidence("b"),
        commands: Vec::new(),
    };
    let unbounded = Package {
        spec: "reader".into(),
        dir: "crates/reader".into(),
        evidence: Evidence {
            content_key: None,
            reason: Some("runtime inputs are undeclared".into()),
        },
        commands: Vec::new(),
    };
    let passed = Passed::from([
        (
            exact.spec.clone(),
            CarryProof::passed(&exact.spec, &exact.evidence, 1.0),
        ),
        (
            changed.spec.clone(),
            CarryProof::passed(&changed.spec, &evidence("c"), 1.0),
        ),
    ]);
    let selected = partition(
        vec![exact, changed, unbounded],
        &passed,
        &BTreeSet::from(["changed".into(), "reader".into()]),
    );
    assert_eq!(
        selected
            .carried
            .iter()
            .map(|(package, _)| package.spec.as_str())
            .collect::<Vec<_>>(),
        ["exact"]
    );
    assert_eq!(
        selected
            .run
            .iter()
            .map(|package| package.spec.as_str())
            .collect::<Vec<_>>(),
        ["changed", "reader"]
    );
}

#[test]
fn one_in_memory_reverse_graph_invalidates_dependents_without_a_reader_process() {
    let package = |spec: &str, key: &str| Package {
        spec: spec.into(),
        dir: format!("crates/{spec}"),
        evidence: evidence(key),
        commands: Vec::new(),
    };
    let changed = package("changed", "a");
    let dependent = package("dependent", "b");
    let independent = package("independent", "c");
    let passed = Passed::from([
        (
            changed.spec.clone(),
            CarryProof::passed(&changed.spec, &evidence("d"), 1.0),
        ),
        (
            dependent.spec.clone(),
            CarryProof::passed(&dependent.spec, &dependent.evidence, 1.0),
        ),
        (
            independent.spec.clone(),
            CarryProof::passed(&independent.spec, &independent.evidence, 1.0),
        ),
    ]);
    let selected = Plan {
        packages: vec![changed, dependent, independent],
        reverse: Some(BTreeMap::from([
            ("changed".into(), BTreeSet::from(["dependent".into()])),
            ("dependent".into(), BTreeSet::new()),
            ("independent".into(), BTreeSet::new()),
        ])),
    }
    .select(&passed);
    assert_eq!(
        selected
            .run
            .iter()
            .map(|package| package.spec.as_str())
            .collect::<Vec<_>>(),
        ["changed", "dependent"]
    );
    assert_eq!(
        selected
            .carried
            .iter()
            .map(|(package, _)| package.spec.as_str())
            .collect::<Vec<_>>(),
        ["independent"]
    );

    let exact = package("exact", "e");
    let prior = CarryProof::passed(&exact.spec, &exact.evidence, 1.0);
    let missing = package("missing", "f");
    let fail_closed = Plan {
        packages: vec![exact, missing],
        reverse: None,
    }
    .select(&Passed::from([("exact".into(), prior)]));
    assert_eq!(fail_closed.run.len(), 2, "an unreadable graph carries none");
    assert!(fail_closed.carried.is_empty());
}

#[test]
fn all_feature_graph_invalidates_a_package_for_a_feature_only_dependency() {
    assert!(
        locked_metadata_args().contains(&"--all-features"),
        "required-feature lint commands need an all-feature dependency over-approximation"
    );

    let dir = aterm_tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(
        a.join("Cargo.toml"),
        "[package]\nname='a'\nversion='0.0.0'\n",
    )
    .unwrap();
    std::fs::write(
        b.join("Cargo.toml"),
        "[package]\nname='b'\nversion='0.0.0'\n",
    )
    .unwrap();
    std::fs::write(a.join("lib.rs"), "").unwrap();
    std::fs::write(b.join("lib.rs"), "").unwrap();
    let a_id = "path+file:///fixture/a#0.0.0";
    let b_id = "path+file:///fixture/b#0.0.0";
    let metadata = aterm_json::json!({
        "packages": [
            {
                "id": a_id,
                "manifest_path": a.join("Cargo.toml"),
                "targets": [{"kind": ["lib"], "src_path": a.join("lib.rs")}],
                "links": null
            },
            {
                "id": b_id,
                "manifest_path": b.join("Cargo.toml"),
                "targets": [{"kind": ["lib"], "src_path": b.join("lib.rs")}],
                "links": null
            }
        ],
        "workspace_members": [a_id, b_id],
        // This A -> B edge exists only when A's optional feature is enabled. Production's
        // `--all-features` metadata must expose it even when A's ordinary pass omits it.
        "resolve": {"nodes": [
            {"id": a_id, "dependencies": [b_id]},
            {"id": b_id, "dependencies": []}
        ]}
    });
    let graph = CargoGraph::parse(&aterm_json::to_string(&metadata).unwrap()).unwrap();
    let roots = graph.closure(&a.canonicalize().unwrap()).unwrap();
    assert!(roots.contains(&b.canonicalize().unwrap()));
    let reverse = graph
        .reverse_packages(
            dir.path(),
            &[("a".into(), "a".into()), ("b".into(), "b".into())],
        )
        .unwrap();
    assert_eq!(reverse["b"], BTreeSet::from(["a".to_owned()]));
    let settings = graph.workspace_settings_digest().unwrap();
    std::fs::write(
        b.join("Cargo.toml"),
        "[package]\nname='b'\nversion='0.0.1'\n",
    )
    .unwrap();
    assert_eq!(settings, graph.workspace_settings_digest().unwrap());
    std::fs::write(
        b.join("Cargo.toml"),
        "[package]\nname='b'\nversion='0.0.1'\n[features]\nextra=[]\n",
    )
    .unwrap();
    assert_ne!(
        settings,
        graph.workspace_settings_digest().unwrap(),
        "workspace feature settings cannot disappear behind an all-feature graph"
    );
}
