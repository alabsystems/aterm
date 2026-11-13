// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::scan_set::test_fixtures::synth_tree;

#[test]
fn the_forge_package_roster_comes_from_both_authoritative_registries() {
    let files = test_fixtures::workspace_manifests(&[]);
    let root = test_fixtures::disk_tree("aterm-fork-packages", "both", &files);
    let packages = fork_packages(&root).expect("the fixture registries parse");
    assert!(packages.contains(&test_fixtures::FIXTURE_FORK.0.to_string()));
    for row in ABSORBED_CRATES {
        assert!(
            packages.contains(&test_fixtures::member_name(row).to_string()),
            "{} is in the absorbed registry: {packages:?}",
            row.path
        );
    }
    let absent = test_fixtures::synth_tree("aterm-fork-packages", "absent", &[]);
    assert!(
        fork_packages(&absent).is_err(),
        "a missing authority must pull Forge fail-closed, not look like an empty roster"
    );
}

fn with_src(mut files: Vec<(String, String)>) -> Vec<(String, String)> {
    let dirs: Vec<String> = files
        .iter()
        .filter_map(|(p, _)| {
            p.strip_suffix("/Cargo.toml")
                .filter(|d| d.starts_with("crates/"))
                .map(str::to_string)
        })
        .collect();
    for d in dirs {
        files.push((format!("{d}/src/lib.rs"), "// synth\n".to_string()));
    }
    files
}

fn derive(name: &str, files: Vec<(String, String)>) -> Result<ScanSet, String> {
    derive_gui_scan_set(&synth_tree("aterm-scan-set", name, &files))
}

#[test]
fn scanned_vendored_crate_missing_src_or_slice_path_fails_closed() {
    let mut files = test_fixtures::workspace_manifests(&[]);
    files.retain(|(p, _)| !p.starts_with("vendor/indexmap/src/"));
    let err = derive("nosrcvendor", with_src(files)).expect_err("must fail");
    assert!(
        err.contains("no `vendor/indexmap/src` directory"),
        "err: {err}"
    );
    let mut files = test_fixtures::workspace_manifests(&[]);
    files.retain(|(p, _)| p != "crates/aterm-window/src/platform/x11.rs");
    let err = derive("staleslice", with_src(files)).expect_err("must fail");
    assert!(
        err.contains("platform slice `linux` path `src/platform/x11.rs`") && err.contains("STALE"),
        "err: {err}"
    );
}

fn reaching_absorbed(dep: &str, path: &str) -> Vec<(String, String)> {
    let mut files = test_fixtures::workspace_manifests(&[]);
    for (rel, contents) in files.iter_mut() {
        if rel == "Cargo.toml" {
            *contents = contents.replace(
                "aterm-types = { path = \"crates/aterm-types\" }\n",
                &format!(
                    "aterm-types = {{ path = \"crates/aterm-types\" }}\n\
                     {dep} = {{ path = \"{path}\" }}\n"
                ),
            );
        }
        if rel == "crates/aterm-gui/Cargo.toml" {
            contents.push_str(&format!("{dep} = {{ workspace = true }}\n"));
        }
    }
    files
}

fn assert_scanned_namespaced(set: &ScanSet, row: &AbsorbedCrate, package: &str) {
    assert_eq!(
        set.absorbed,
        vec![(package.to_string(), row.path.to_string())]
    );
    assert!(
        !set.scan_dirs.contains(&format!("{}/src", row.path)),
        "an absorbed crate is not scanned unnamespaced: {:?}",
        set.scan_dirs
    );
    let scanned = set
        .vendored_scanned
        .iter()
        .find(|v| v.package == package)
        .expect("the absorbed crate is scanned namespaced");
    assert_eq!(scanned.namespace, row.namespace);
    assert_eq!(scanned.crate_dir, row.path);
    assert_eq!(scanned.platform_slices.len(), row.platform_slices.len());
}

#[test]
fn a_renamed_absorbed_member_is_scanned_namespaced_not_as_a_member() {
    let row = &ABSORBED_CRATES[0];
    let member = test_fixtures::member_name(row);
    let files = reaching_absorbed(member, row.path);
    let set = derive("absorbedmember", with_src(files)).expect("derivation must succeed");
    assert_scanned_namespaced(&set, row, member);
}

#[test]
fn an_absorbed_patch_target_is_scanned_namespaced_not_as_a_member() {
    let row = &ABSORBED_CRATES[0];
    let mut files = reaching_absorbed(row.upstream, row.path);
    for (rel, contents) in files.iter_mut() {
        if rel == "Cargo.toml" {
            contents.push_str(&format!(
                "{} = {{ path = \"{}\" }}\n",
                row.upstream, row.path
            ));
        }
        if *rel == format!("{}/Cargo.toml", row.path) {
            *contents = format!("[package]\nname = \"{}\"\n", row.upstream);
        }
    }
    let set = derive("absorbedpatch", with_src(files)).expect("derivation must succeed");
    assert_scanned_namespaced(&set, row, row.upstream);
}

#[test]
fn the_absorbed_registry_is_fail_closed_both_ways() {
    let absorbed = &ABSORBED_CRATES[0];
    let mut files = test_fixtures::workspace_manifests(&[]);
    for (path, contents) in files.iter_mut() {
        if path == "Cargo.toml" {
            contents.push_str("absorbed-new = { path = \"crates/aterm-absorbed-new\" }\n");
        }
        if path == "NOTICE" {
            contents.push_str("- absorbed-new (`crates/aterm-absorbed-new/`)\n");
        }
    }
    files.push((
        "crates/aterm-absorbed-new/Cargo.toml".to_string(),
        "[package]\nname = \"absorbed-new\"\n".to_string(),
    ));
    files.push((
        "crates/aterm-absorbed-new/LICENSE-MIT".to_string(),
        "MIT\n".to_string(),
    ));
    let err = derive("unrowed", with_src(files)).expect_err("must fail");
    assert!(
        err.contains("`absorbed-new`") && err.contains("no ABSORBED_CRATES row"),
        "err: {err}"
    );
    let mut files = test_fixtures::workspace_manifests(&[]);
    for (path, contents) in files.iter_mut() {
        if path == "NOTICE" {
            *contents = contents.replace(
                &format!("- {} (`{}/`)\n", absorbed.upstream, absorbed.path),
                "",
            );
        }
    }
    let err = derive("staleabsorbed", with_src(files)).expect_err("must fail");
    assert!(
        err.contains(&format!("ABSORBED_CRATES entry `{}`", absorbed.upstream))
            && err.contains("STALE"),
        "err: {err}"
    );
}

#[test]
fn a_first_party_patch_target_is_recorded_and_never_review_registered() {
    let mut files = test_fixtures::workspace_manifests(&[("aterm-shim", "")]);
    for (path, contents) in files.iter_mut() {
        if path == "Cargo.toml" {
            contents.push_str("shimmed = { path = \"crates/aterm-shim\" }\n");
        }
    }
    let set = derive("firstparty", with_src(files)).expect("derivation must succeed");
    assert_eq!(
        set.first_party_patches,
        vec![("shimmed".to_string(), "crates/aterm-shim".to_string())]
    );
    assert!(set.vendored_scanned.iter().all(|v| v.package != "shimmed"));
}

#[test]
fn a_renamed_patch_row_is_recorded_under_the_package_it_replaces() {
    let mut files = test_fixtures::workspace_manifests(&[("aterm-shim-052", "")]);
    for (path, contents) in files.iter_mut() {
        if path == "Cargo.toml" {
            contents.push_str(
                "shimmed-052 = { package = \"shimmed\", path = \"crates/aterm-shim-052\" }\n",
            );
        }
    }
    let set = derive("renamed", with_src(files)).expect("derivation must succeed");
    assert_eq!(
        set.first_party_patches,
        vec![("shimmed".to_string(), "crates/aterm-shim-052".to_string())]
    );
}

#[test]
fn a_patch_target_outside_vendor_and_crates_fails_closed() {
    let mut files = test_fixtures::workspace_manifests(&[]);
    for (path, contents) in files.iter_mut() {
        if path == "Cargo.toml" {
            contents.push_str("elsewhere = { path = \"third_party/elsewhere\" }\n");
        }
    }
    files.push((
        "third_party/elsewhere/Cargo.toml".to_string(),
        "[package]\nname = \"elsewhere\"\n".to_string(),
    ));
    let err = derive("oddpatch", with_src(files)).expect_err("must fail");
    assert!(
        err.contains("under neither `vendor/`") && err.contains("fail-closed"),
        "err: {err}"
    );
}

#[test]
fn a_review_row_for_a_first_party_patch_target_is_refused() {
    let (package, path, _) = test_fixtures::FIXTURE_FORK;
    let mut files = test_fixtures::workspace_manifests(&[("aterm-shim", "")]);
    for (rel, contents) in files.iter_mut() {
        if rel == "Cargo.toml" {
            *contents = contents.replace(
                &format!("{package} = {{ path = \"{path}\" }}"),
                &format!("{package} = {{ path = \"crates/aterm-shim\" }}"),
            );
        }
    }
    let err = derive("misfiled", with_src(files)).expect_err("must fail");
    assert!(
        err.contains(&format!("`{package}` points at first-party"))
            && err.contains("ALSO has a `[[fork]]` block"),
        "err: {err}"
    );
}

#[test]
fn a_redistribution_under_crates_is_refused_not_called_first_party() {
    let root = synth_tree(
        "aterm-scan-set",
        "redistribution",
        &[
            (
                "crates/aterm-fork/LICENSE-MIT".to_string(),
                "MIT\n".to_string(),
            ),
            (
                "crates/aterm-noticed/src/lib.rs".to_string(),
                "// derived\n".to_string(),
            ),
            (
                "NOTICE".to_string(),
                "crates/aterm-noticed is derived from upstream\n".to_string(),
            ),
        ],
    );
    let license = classify_patch_target("fork", "crates/aterm-fork", &root);
    let notice = classify_patch_target("noticed", "crates/aterm-noticed", &root);
    root.write("crates/aterm-noticed/LICENSE", "upstream\n");
    let both = classify_patch_target("noticed", "crates/aterm-noticed", &root);
    assert_eq!(both, Ok(PatchTargetKind::Absorbed));
    let license = license.expect_err("a retained license is a redistribution");
    assert!(
        license.contains("crates/aterm-fork/LICENSE-MIT"),
        "{license}"
    );
    let notice = notice.expect_err("a NOTICE line is a redistribution");
    assert!(notice.contains("NOTICE"), "{notice}");
}

#[test]
fn a_genuine_first_party_crate_is_not_mistaken_for_a_redistribution() {
    let root = synth_tree(
        "aterm-scan-set",
        "own",
        &[
            (
                "crates/aterm-own/src/lib.rs".to_string(),
                "// original\n".to_string(),
            ),
            (
                "NOTICE".to_string(),
                "crates/aterm-other is derived from upstream\n".to_string(),
            ),
        ],
    );
    let evidence = redistribution_evidence(&root, "crates/aterm-own");
    let kind = classify_patch_target("own", "crates/aterm-own", &root);
    assert_eq!(evidence, None);
    assert_eq!(kind, Ok(PatchTargetKind::FirstParty));
}

#[test]
fn direct_workspace_and_dotted_path_deps_are_followed() {
    let mut files = test_fixtures::workspace_manifests(&[
        (
            "aterm-a",
            "[dependencies]\naterm-b = { path = \"../aterm-b\" }\n",
        ),
        ("aterm-c", ""),
    ]);
    files.push((
        "crates/aterm-b/Cargo.toml".to_string(),
        "[package]\nname = \"aterm-b\"\n[dependencies]\naterm-types.workspace = true\n".to_string(),
    ));
    let set = derive("chain", with_src(files)).expect("derivation");
    assert_eq!(
        set.scan_dirs,
        vec![
            "crates/aterm-a/src",
            "crates/aterm-b/src",
            "crates/aterm-c/src",
            "crates/aterm-gui/src",
            "crates/aterm-types/src",
        ]
    );
}

#[test]
fn a_nested_workspaces_dependencies_and_features_belong_to_that_workspace() {
    for explicit in ["", "workspace = \"../..\"\n"] {
        let mut files = test_fixtures::workspace_manifests(&[(
            "aterm-a",
            "[dependencies]\nstart = { path = \"../../vendor/nested/crates/start\" }\n",
        )]);
        let outer = files
            .iter_mut()
            .find(|(path, _)| path == "Cargo.toml")
            .unwrap();
        outer.1 = outer.1.replacen(
            "[workspace.dependencies]\n",
            "[workspace.dependencies]\nshared = { path = \"wrong-root-package\" }\n",
            1,
        );
        files.extend([
            ("vendor/nested/Cargo.toml".into(), "[workspace]\nmembers = [\"crates/*\"]\n[workspace.dependencies]\nshared = { path = \"crates/leaf\", features = [\"wanted\"] }\n".into()),
            ("vendor/nested/crates/start/Cargo.toml".into(), format!("[package]\nname = \"start\"\n{explicit}[dependencies]\nshared.workspace = true\n")),
            ("vendor/nested/crates/leaf/Cargo.toml".into(), "[package]\nname = \"leaf\"\n[features]\nwanted = [\"dep:extra\"]\n[dependencies]\nextra = { path = \"../extra\", optional = true }\n".into()),
            ("vendor/nested/crates/extra/Cargo.toml".into(), "[package]\nname = \"extra\"\n".into()),
            ("vendor/nested/crates/start/src/lib.rs".into(), "// start\n".into()),
            ("vendor/nested/crates/leaf/src/lib.rs".into(), "// leaf\n".into()),
            ("vendor/nested/crates/extra/src/lib.rs".into(), "// extra\n".into()),
        ]);
        let root = synth_tree("aterm-scan-set", "nested-workspace", &with_src(files));
        let set = derive_gui_scan_set(&root).expect("nested inheritance");
        assert!(
            set.scan_dirs
                .contains(&"vendor/nested/crates/leaf/src".into())
        );
        assert!(
            set.scan_dirs
                .contains(&"vendor/nested/crates/extra/src".into())
        );
        assert!(
            external_dependency_names(&root, "vendor/nested/crates/start")
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn a_missing_nested_workspace_entry_does_not_fall_back_to_the_outer_workspace() {
    let mut files = test_fixtures::workspace_manifests(&[(
        "aterm-a",
        "[dependencies]\nstart = { path = \"../../vendor/nested/crates/start\" }\n",
    )]);
    files.extend([
        (
            "vendor/nested/Cargo.toml".into(),
            "[workspace]\nmembers = [\"crates/*\"]\n".into(),
        ),
        (
            "vendor/nested/crates/start/Cargo.toml".into(),
            "[package]\nname = \"start\"\n[dependencies]\naterm-types.workspace = true\n".into(),
        ),
        (
            "vendor/nested/crates/start/src/lib.rs".into(),
            "// start\n".into(),
        ),
    ]);
    let error = derive("missing-nested-entry", with_src(files)).expect_err("no outer inheritance");
    assert!(
        error.contains("aterm-types") && error.contains("no such entry"),
        "{error}"
    );
}

#[test]
fn dev_and_build_deps_are_excluded() {
    let mut files = test_fixtures::workspace_manifests(&[(
        "aterm-a",
        "[dev-dependencies]\naterm-devonly = { path = \"../aterm-devonly\" }\n\
         [build-dependencies]\naterm-buildonly = { path = \"../aterm-buildonly\" }\n",
    )]);
    for name in ["aterm-devonly", "aterm-buildonly"] {
        files.push((
            format!("crates/{name}/Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        ));
    }
    let set = derive("devbuild", with_src(files)).expect("derivation");
    assert!(
        !set.scan_dirs.iter().any(|d| d.contains("only")),
        "dev-/build-deps must never enter the process closure: {:?}",
        set.scan_dirs
    );
}

#[test]
fn cfg_target_path_deps_are_included_platform_independently() {
    let mut files = test_fixtures::workspace_manifests(&[(
        "aterm-a",
        "[target.'cfg(windows)'.dependencies]\n\
         aterm-winonly = { path = \"../aterm-winonly\" }\n\
         [target.'cfg(unix)'.dev-dependencies]\n\
         aterm-devonly = { path = \"../aterm-devonly\" }\n",
    )]);
    for name in ["aterm-winonly", "aterm-devonly"] {
        files.push((
            format!("crates/{name}/Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        ));
    }
    let set = derive("cfgdeps", with_src(files)).expect("derivation");
    assert!(
        set.scan_dirs
            .contains(&"crates/aterm-winonly/src".to_string()),
        "cfg-target NORMAL deps are IN: {:?}",
        set.scan_dirs
    );
    assert!(
        !set.scan_dirs
            .contains(&"crates/aterm-devonly/src".to_string()),
        "cfg-target DEV deps stay out: {:?}",
        set.scan_dirs
    );
    assert!(set.unlinked.is_empty(), "{:?}", set.unlinked);
}

#[test]
fn a_target_table_the_target_cannot_link_is_left_out_of_its_closure() {
    let mut files = test_fixtures::workspace_manifests(&[(
        "aterm-a",
        "[target.'cfg(windows)'.dependencies]\n\
         aterm-winonly = { path = \"../aterm-winonly\" }\n\
         [target.\"cfg(unix)\".dependencies.aterm-unixonly]\n\
         path = \"../aterm-unixonly\"\n\
         [target.x86_64-pc-windows-msvc.dependencies]\n\
         aterm-triple = { path = \"../aterm-triple\" }\n\
         [target.'cfg(target_arch = \"wasm32\")'.dependencies]\n\
         aterm-web = { path = \"../aterm-web\" }\n",
    )]);
    let gated = ["aterm-winonly", "aterm-unixonly", "aterm-triple"];
    for name in gated.iter().chain(&["aterm-web"]) {
        files.push((
            format!("crates/{name}/Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        ));
    }
    let root = synth_tree("aterm-scan-set", "linked", &with_src(files));
    let set = derive_linked_scan_set(&root, &[GUI_ROOT_CRATE], &|key| {
        key == "cfg(target_arch = \"wasm32\")"
    })
    .expect("derivation");
    for name in gated {
        assert!(
            !set.scan_dirs.contains(&format!("crates/{name}/src")),
            "{name} is linked only off the target: {:?}",
            set.scan_dirs
        );
    }
    assert!(
        set.scan_dirs.contains(&"crates/aterm-web/src".to_string()),
        "the table the target links is followed: {:?}",
        set.scan_dirs
    );
    assert_eq!(
        set.unlinked,
        vec![
            (
                "crates/aterm-a".to_string(),
                "aterm-triple".to_string(),
                "x86_64-pc-windows-msvc".to_string()
            ),
            (
                "crates/aterm-a".to_string(),
                "aterm-unixonly".to_string(),
                "cfg(unix)".to_string()
            ),
            (
                "crates/aterm-a".to_string(),
                "aterm-winonly".to_string(),
                "cfg(windows)".to_string()
            ),
        ]
    );
    let every = derive_gui_scan_set(&root).expect("derivation");
    for name in gated {
        assert!(
            every.scan_dirs.contains(&format!("crates/{name}/src")),
            "CONTROL: platform-independent, {name} is in: {:?}",
            every.scan_dirs
        );
    }
}

#[test]
fn the_one_table_dependency_form_is_followed_like_the_inline_one() {
    let mut files = test_fixtures::workspace_manifests(&[(
        "aterm-a",
        "[dependencies.aterm-b]\n\
         path = \"../aterm-b\"\n\
         [dependencies.rwh_06]\n\
         version = \"0.6\"\n\
         features = [\n    \"std\",\n]\n\
         package = \"raw-window-handle\"\n\
         [target.'cfg(all(unix, not(any(target_os = \"ios\", target_os = \"macos\"))))'.dependencies.aterm-c]\n\
         path = \"../aterm-c\"\n\
         [dev-dependencies.aterm-devonly]\n\
         path = \"../aterm-devonly\"\n",
    )]);
    for name in ["aterm-b", "aterm-c", "aterm-devonly"] {
        files.push((
            format!("crates/{name}/Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        ));
    }
    let set = derive("onetable", with_src(files)).expect("derivation");
    for followed in ["crates/aterm-b/src", "crates/aterm-c/src"] {
        assert!(
            set.scan_dirs.contains(&followed.to_string()),
            "{followed} is a normal dependency: {:?}",
            set.scan_dirs
        );
    }
    assert!(
        !set.scan_dirs
            .contains(&"crates/aterm-devonly/src".to_string()),
        "a dotted dev dependency stays out: {:?}",
        set.scan_dirs
    );
    let mut files = test_fixtures::workspace_manifests(&[(
        "aterm-a",
        "[dependencies.renamed]\npath = \"../aterm-b\"\npackage = \"aterm-b\"\n",
    )]);
    files.push((
        "crates/aterm-b/Cargo.toml".to_string(),
        "[package]\nname = \"aterm-b\"\n".to_string(),
    ));
    let set = derive("renamedpath", with_src(files)).expect("derivation");
    assert!(
        set.scan_dirs.contains(&"crates/aterm-b/src".to_string()),
        "a renamed path dependency is followed by its path: {:?}",
        set.scan_dirs
    );
}

#[test]
fn a_renamed_path_dependency_is_walked_by_path_and_activated_by_key() {
    let manifests = |gui_feature: &str| {
        let (patch_table, mut files) = test_fixtures::patch_fixture();
        let mut root = String::from("[workspace]\nmembers = [\"crates/*\"]\n");
        root.push_str("[workspace.dependencies]\n");
        root.push_str("serde = { package = \"aterm-serde\", path = \"crates/aterm-serde\" }\n");
        root.push_str(&patch_table);
        files.push(("Cargo.toml".to_string(), root));
        files.push((
            "crates/aterm-gui/Cargo.toml".to_string(),
            format!(
                "[package]\nname = \"aterm-gui\"\n\
                 [features]\ndefault = [\"persist\"]\npersist = [\"{gui_feature}\"]\n\
                 [dependencies]\nserde = {{ workspace = true, optional = true }}\n\
                 quiet = {{ package = \"aterm-off\", path = \"../aterm-off\", optional = true }}\n"
            ),
        ));
        for n in ["aterm-serde", "aterm-off"] {
            files.push((
                format!("crates/{n}/Cargo.toml"),
                format!("[package]\nname = \"{n}\"\n"),
            ));
        }
        with_src(files)
    };
    let set = derive("renamedws", manifests("dep:serde")).expect("derivation");
    assert!(
        set.scan_dirs
            .contains(&"crates/aterm-serde/src".to_string()),
        "default -> persist -> dep:serde must take in crates/aterm-serde: {:?}",
        set.scan_dirs
    );
    assert!(
        !set.scan_dirs.contains(&"crates/aterm-off/src".to_string()),
        "an unactivated renamed optional dependency stays out: {:?}",
        set.scan_dirs
    );
    let err = derive("renamedwspkg", manifests("dep:aterm-serde")).expect_err("must fail");
    assert!(err.contains("names no dependency"), "err: {err}");
}

#[test]
fn optional_deps_follow_default_feature_activation() {
    let (patch_table, mut files) = test_fixtures::patch_fixture();
    let mut root = String::from("[workspace]\nmembers = [\"crates/*\"]\n");
    root.push_str("[workspace.dependencies]\n");
    for n in ["aterm-mid", "aterm-on", "aterm-off", "aterm-edge"] {
        root.push_str(&format!("{n} = {{ path = \"crates/{n}\" }}\n"));
    }
    root.push_str(&patch_table);
    files.push(("Cargo.toml".to_string(), root));
    files.push((
        "crates/aterm-gui/Cargo.toml".to_string(),
        "[package]\nname = \"aterm-gui\"\n\
         [features]\ndefault = [\"fancy\"]\nfancy = [\"aterm-mid/glitter\"]\n\
         [dependencies]\naterm-mid = { workspace = true, features = [\"edgefeat\"] }\n"
            .to_string(),
    ));
    files.push((
        "crates/aterm-mid/Cargo.toml".to_string(),
        "[package]\nname = \"aterm-mid\"\n\
         [features]\nglitter = [\"dep:aterm-on\"]\nanchors = [\"dep:aterm-off\"]\n\
         edgefeat = [\"aterm-edge\"]\n\
         [dependencies]\naterm-on = { workspace = true, optional = true }\n\
         aterm-off = { workspace = true, optional = true }\n\
         aterm-edge = { workspace = true, optional = true }\n"
            .to_string(),
    ));
    for n in ["aterm-on", "aterm-off", "aterm-edge"] {
        files.push((
            format!("crates/{n}/Cargo.toml"),
            format!("[package]\nname = \"{n}\"\n"),
        ));
    }
    let set = derive("features", with_src(files)).expect("derivation");
    assert!(
        set.scan_dirs.contains(&"crates/aterm-on/src".to_string()),
        "default->fancy->aterm-mid/glitter->dep:aterm-on must be IN: {:?}",
        set.scan_dirs
    );
    assert!(
        set.scan_dirs.contains(&"crates/aterm-edge/src".to_string()),
        "edge features=[\"edgefeat\"] -> implicit optional dep must be IN: {:?}",
        set.scan_dirs
    );
    assert!(
        !set.scan_dirs.contains(&"crates/aterm-off/src".to_string()),
        "an unactivated optional dep (a feature left off) must stay OUT: {:?}",
        set.scan_dirs
    );
}

#[test]
fn proc_macro_crates_are_classified_out_and_reported() {
    let files = test_fixtures::workspace_manifests(&[(
        "aterm-derive",
        "[lib]\nproc-macro = true\n\
         [dependencies]\naterm-macdep = { path = \"../aterm-macdep\" }\n",
    )]);
    let set = derive("procmacro", with_src(files)).expect("derivation");
    assert!(
        !set.scan_dirs.iter().any(|d| d.contains("aterm-derive")),
        "proc-macro crates must not be scanned: {:?}",
        set.scan_dirs
    );
    assert!(
        !set.scan_dirs.iter().any(|d| d.contains("aterm-macdep")),
        "proc-macro deps must not be followed: {:?}",
        set.scan_dirs
    );
    assert_eq!(
        set.proc_macros,
        vec![(
            "aterm-derive".to_string(),
            "crates/aterm-derive".to_string()
        )],
        "the exclusion must be REPORTED, never silent"
    );
}

#[test]
fn missing_dep_manifest_and_missing_src_fail_closed() {
    let files = test_fixtures::workspace_manifests(&[(
        "aterm-a",
        "[dependencies]\naterm-ghost = { path = \"../aterm-ghost\" }\n",
    )]);
    let err = derive("ghost", with_src(files)).expect_err("must fail");
    assert!(err.contains("aterm-ghost/Cargo.toml"), "err: {err}");
    let files = test_fixtures::workspace_manifests(&[("aterm-nosrc", "")]);
    let mut files = with_src(files);
    files.retain(|(p, _)| p != "crates/aterm-nosrc/src/lib.rs");
    let err = derive("nosrc", files).expect_err("must fail");
    assert!(
        err.contains("no src/ directory"),
        "the drift guard must hard-error: {err}"
    );
}

#[test]
fn unclassifiable_dep_specs_fail_closed() {
    let files = test_fixtures::workspace_manifests(&[(
        "aterm-a",
        "[dependencies]\nweird = { git = \"https://example.com/x\" }\n",
    )]);
    let err = derive("gitdep", with_src(files)).expect_err("must fail");
    assert!(err.contains("cannot soundly classify"), "err: {err}");
    let files = test_fixtures::workspace_manifests(&[(
        "aterm-a",
        "[features]\ndefault = [\"nonexistent-thing\"]\n",
    )]);
    let err = derive("badfeat", with_src(files)).expect_err("must fail");
    assert!(
        err.contains("neither a feature nor an optional dependency"),
        "err: {err}"
    );
}

#[test]
fn pinned_git_edges_require_the_exact_lock_and_keep_their_package_identity() {
    let hash = "1234567890123456789012345678901234567890";
    let url = "https://github.com/example/leaf.git";
    let spec = format!("{{ package = \"leaf\", git = \"{url}\", rev = \"{hash}\" }}");
    let locked = format!(
        "[[package]]\nname = \"leaf\"\nversion = \"1.0.0\"\nsource = \"git+{url}?rev={hash}#{hash}\"\n"
    );
    let fixture = |name: &str, specification: &str, lock: Option<&str>, inherited: bool| {
        let dependency = if inherited {
            "{ workspace = true }"
        } else {
            specification
        };
        let manifest = format!("[dependencies]\nexternal = {dependency}\n");
        let mut files = test_fixtures::workspace_manifests(&[("aterm-a", &manifest)]);
        if inherited {
            for (path, content) in &mut files {
                if path == "Cargo.toml" {
                    *content = content.replace(
                        "[workspace.dependencies]\n",
                        &format!("[workspace.dependencies]\nexternal = {specification}\n"),
                    );
                }
            }
        }
        if let Some(lock) = lock {
            files.push(("Cargo.lock".into(), lock.into()));
        }
        synth_tree("aterm-census-git", name, &with_src(files))
    };
    for inherited in [false, true] {
        let root = fixture(
            if inherited { "inherited" } else { "direct" },
            &spec,
            Some(&locked),
            inherited,
        );
        derive_gui_scan_set(&root).expect("the exact immutable external edge is modeled");
        assert_eq!(
            external_dependency_names(&root, "crates/aterm-a").unwrap(),
            ["leaf"]
        );
    }
    for (name, specification, lock) in [
        ("missing-lock", spec.clone(), None),
        (
            "wrong-lock",
            spec.clone(),
            Some(locked.replace(hash, "9876543210987654321098765432109876543210")),
        ),
        (
            "ambiguous-lock",
            spec.clone(),
            Some(format!("{locked}{locked}")),
        ),
        ("mutable", spec.replace(hash, "main"), Some(locked.clone())),
        (
            "no-rev",
            format!("{{ git = \"{url}\" }}"),
            Some(locked.clone()),
        ),
        (
            "mixed-path",
            format!("{{ git = \"{url}\", rev = \"{hash}\", path = \"../leaf\" }}"),
            Some(locked.clone()),
        ),
    ] {
        let root = fixture(name, &specification, lock.as_deref(), true);
        assert!(derive_gui_scan_set(&root).is_err(), "accepted {name}");
    }
}

#[test]
fn the_review_set_is_the_fork_ledger() {
    let repo = test_fixtures::repo_root();
    for fork in reviewed_forks(&repo).expect("the real ledger reads") {
        assert!(
            repo.join(&fork.path).join("Cargo.toml").is_file(),
            "{fork:?}"
        );
    }
    let read = |name: &str, ledger: &str| {
        reviewed_forks(&synth_tree(
            "aterm-scan-set",
            name,
            &[(FORK_LEDGER.to_string(), ledger.to_string())],
        ))
    };
    let table = "[[fork]]\nname = \"a\"\npath = \"vendor/a\"\n\
                 [fork.census]\nmode = \"scanned\"\nnamespace = \"a_ns\"\n";
    assert_eq!(
        read("ledger-table", table).expect("a [fork.census] table reads"),
        [ReviewedFork {
            package: "a".to_string(),
            path: "vendor/a".to_string(),
            namespace: "a_ns",
        }]
    );
    let good = test_fixtures::ledger(&[("a", "vendor/a", "a_ns")]);
    for (name, ledger, says) in [
        (
            "ledger-no-ns",
            good.replace("census.namespace = \"a_ns\"\n", ""),
            "no census.namespace",
        ),
        (
            "ledger-mode",
            good.replace("\"scanned\"", "\"build-dep-only\""),
            "census.mode Some(\"build-dep-only\")",
        ),
        (
            "ledger-no-path",
            good.replace("path = \"vendor/a\"\n", ""),
            "no `name` or no `path`",
        ),
    ] {
        let err = read(name, &ledger).expect_err("must fail");
        assert!(err.contains(says), "{name}: {err}");
    }
    let root = synth_tree("aterm-scan-set", "ledger-absent", &[]);
    assert_eq!(reviewed_forks(&root), Ok(Vec::new()));
}

#[test]
fn exclusion_list_is_fail_closed_both_ways() {
    let mut files = test_fixtures::workspace_manifests(&[]);
    for (p, c) in &mut files {
        if p == "Cargo.toml" {
            c.push_str("shiny-new-fork = { path = \"vendor/shiny-new-fork\" }\n");
        }
    }
    files.push((
        "vendor/shiny-new-fork/Cargo.toml".to_string(),
        "[package]\nname = \"shiny-new-fork\"\n".to_string(),
    ));
    let err = derive("unreviewed", with_src(files)).expect_err("must fail");
    assert!(
        err.contains("has NO `[[fork]]` block in vendor/forge.toml"),
        "err: {err}"
    );
    let mut files = test_fixtures::workspace_manifests(&[]);
    for (p, c) in &mut files {
        if p == "Cargo.toml" {
            *c = c.replace("indexmap = { path = \"vendor/indexmap\" }\n", "");
        }
    }
    let err = derive("stale", with_src(files)).expect_err("must fail");
    assert!(
        err.contains("no longer in the root [patch] table") && err.contains("indexmap"),
        "err: {err}"
    );
    let mut files = test_fixtures::workspace_manifests(&[]);
    files.retain(|(p, _)| p != "vendor/indexmap/Cargo.toml");
    let err = derive("gone", with_src(files)).expect_err("must fail");
    assert!(
        err.contains("no Cargo.toml on disk") && err.contains("indexmap"),
        "err: {err}"
    );
}

#[test]
fn default_features_false_suppresses_and_union_reenables() {
    let (patch_table, base) = test_fixtures::patch_fixture();
    let mk = |second_edge: &str| {
        let mut files = base.clone();
        let mut root = String::from("[workspace]\nmembers = [\"crates/*\"]\n");
        root.push_str("[workspace.dependencies]\n");
        for n in ["aterm-mid", "aterm-other", "aterm-opt"] {
            root.push_str(&format!("{n} = {{ path = \"crates/{n}\" }}\n"));
        }
        root.push_str(&patch_table);
        files.push(("Cargo.toml".to_string(), root));
        files.push((
            "crates/aterm-gui/Cargo.toml".to_string(),
            format!(
                "[package]\nname = \"aterm-gui\"\n[dependencies]\n\
                 aterm-mid = {{ workspace = true, default-features = false }}\n\
                 {second_edge}"
            ),
        ));
        files.push((
            "crates/aterm-other/Cargo.toml".to_string(),
            "[package]\nname = \"aterm-other\"\n[dependencies]\n\
             aterm-mid = { workspace = true }\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-mid/Cargo.toml".to_string(),
            "[package]\nname = \"aterm-mid\"\n\
             [features]\ndefault = [\"dep:aterm-opt\"]\n\
             [dependencies]\naterm-opt = { workspace = true, optional = true }\n"
                .to_string(),
        ));
        files.push((
            "crates/aterm-opt/Cargo.toml".to_string(),
            "[package]\nname = \"aterm-opt\"\n".to_string(),
        ));
        files
    };
    let set = derive("dfoff", with_src(mk(""))).expect("derivation");
    assert!(
        !set.scan_dirs.contains(&"crates/aterm-opt/src".to_string()),
        "default-features = false must suppress the default-activated \
         optional dep: {:?}",
        set.scan_dirs
    );
    let set = derive(
        "dfunion",
        with_src(mk("aterm-other = { workspace = true }\n")),
    )
    .expect("derivation");
    assert!(
        set.scan_dirs.contains(&"crates/aterm-opt/src".to_string()),
        "a second defaults-on edge must re-enable via union: {:?}",
        set.scan_dirs
    );
}
