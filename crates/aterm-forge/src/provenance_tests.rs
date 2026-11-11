// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn git_ownership_requires_a_reviewed_component_and_an_immutable_resolved_pin() {
    let repository = "https://github.com/our-account/aterm";
    let hash = "1234567890123456789012345678901234567890";
    let source = |owner: &str, query: &str, resolved: &str| {
        format!("git+https://github.com/{owner}/trust.git?{query}#{resolved}")
    };
    assert!(pinned_first_party_git(
        repository,
        "trust-build-cpu",
        &source("our-account", &format!("rev={hash}"), hash)
    ));
    let ring_fork = format!("git+https://github.com/our-account/ring.git?rev={hash}#{hash}");
    assert!(!pinned_first_party_git(repository, "ring", &ring_fork));
    assert!(!pinned_first_party_git(
        repository,
        "trust-build-cpu",
        &ring_fork
    ));
    assert!(!pinned_first_party_git(
        repository,
        "ring",
        &source("our-account", &format!("rev={hash}"), hash)
    ));
    for candidate in [
        source("other-account", &format!("rev={hash}"), hash),
        source("our-account-copy", &format!("rev={hash}"), hash),
        source("our-account", "branch=main", hash),
        source("our-account", "rev=1234567", hash),
        source(
            "our-account",
            &format!("rev={hash}"),
            "9876543210987654321098765432109876543210",
        ),
        format!("git+https://github.com.evil/our-account/core?rev={hash}#{hash}"),
        format!("git+https://our-account@github.com/other/core?rev={hash}#{hash}"),
    ] {
        assert!(
            !pinned_first_party_git(repository, "trust-build-cpu", &candidate),
            "accepted {candidate}"
        );
    }
}

#[test]
fn path_values_are_read_out_of_a_manifest_and_relative_parts_resolve() {
    let paths = |manifest: &str| {
        dependency_paths(
            &manifest.parse().unwrap(),
            None,
            Path::new("/w"),
            Path::new("/w/crates/member"),
        )
    };
    assert_eq!(
        paths(
            "[dependencies]\nastream-broker = { path = \"../../vendor/astream/crates/astream-broker\", features = [\"cap\"] }\nother.workspace = true\n"
        ),
        vec![PathBuf::from("/w/vendor/astream/crates/astream-broker")]
    );
    assert_eq!(
        normalize(Path::new(
            "/w/crates/aterm-link/../../vendor/astream/crates/x"
        )),
        PathBuf::from("/w/vendor/astream/crates/x")
    );
    assert_eq!(
        paths(
            "[dependencies]\nx = \"1\"\nxpath = \"vendor/fake\"\n# path = \"vendor/commented\"\n"
        ),
        Vec::<PathBuf>::new()
    );
}

#[test]
fn inherited_paths_follow_aliases_and_workspace_relative_paths() {
    let workspace = "[workspace.dependencies]\ncpu = { package = 'trust-build-cpu', path = 'vendor/trust-build-cpu' }\nunused = { path = 'vendor/unused' }\nregistry = '1'\n"
        .parse::<DocumentMut>()
        .unwrap();
    let manifest = "[dependencies]\ncpu.workspace = true\nregistry.workspace = true\nmissing.workspace = true\n[build-dependencies.cpu]\nworkspace = true\n[target.'cfg(unix)'.dev-dependencies]\ncpu = { workspace = true }\n"
        .parse::<DocumentMut>()
        .unwrap();
    assert_eq!(
        dependency_paths(
            &manifest,
            Some(&workspace),
            Path::new("/w"),
            Path::new("/w/crates/member"),
        ),
        vec![PathBuf::from("/w/vendor/trust-build-cpu"); 3]
    );
    assert!(
        dependency_paths(
            &manifest,
            None,
            Path::new("/w"),
            Path::new("/w/crates/member"),
        )
        .is_empty()
    );
}

#[test]
fn only_dependency_tables_establish_consumers() {
    let manifest = "[package]\nname = 'member'\n[package.metadata]\npath = '../../vendor/fake'\n[lib]\npath = '../../vendor/not-a-dependency/lib.rs'\n[dependencies.direct]\npath = '../../vendor/direct'\n[target.'cfg(unix)'.build-dependencies]\nplatform = { path = '../../vendor/platform' }\n"
        .parse::<DocumentMut>()
        .unwrap();
    assert_eq!(
        dependency_paths(
            &manifest,
            None,
            Path::new("/w"),
            Path::new("/w/crates/member"),
        ),
        vec![
            PathBuf::from("/w/vendor/direct"),
            PathBuf::from("/w/vendor/platform")
        ]
    );
}

#[test]
fn an_unused_workspace_path_is_not_a_consumer() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let member = root.join("crates/member");
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace.dependencies]\ncpu = { path = 'vendor/trust-build-cpu' }\n",
    )
    .unwrap();
    let manifest = member.join("Cargo.toml");
    std::fs::write(&manifest, "[dependencies]\ncpu.workspace = true\n").unwrap();
    assert_eq!(path_dependants(root, "vendor/trust-build-cpu"), ["member"]);
    std::fs::write(
        &manifest,
        "[package.metadata]\npath = '../../vendor/trust-build-cpu'\n",
    )
    .unwrap();
    assert!(path_dependants(root, "vendor/trust-build-cpu").is_empty());
}

#[test]
fn a_roster_root_reached_through_a_consumed_one_is_consumed() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    let member = root.join("crates/member");
    let cpu = root.join("vendor/trust-build-cpu");
    let sha2 = root.join("vendor/trust-sha2");
    for dir in [&member, &cpu, &sha2] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(
        cpu.join("Cargo.toml"),
        "[dependencies]\nsha2 = { package = 'trust-sha2', path = '../trust-sha2' }\n",
    )
    .unwrap();
    std::fs::write(
        sha2.join("Cargo.toml"),
        "[dependencies]\ncpu = { path = '../trust-build-cpu' }\n",
    )
    .unwrap();
    let manifest = member.join("Cargo.toml");
    // Unconsumed, the two roots reach each other and nothing else: neither is consumed.
    std::fs::write(&manifest, "[package]\nname = 'member'\n").unwrap();
    assert!(path_dependants(root, "vendor/trust-sha2").is_empty());
    assert!(path_dependants(root, "vendor/trust-build-cpu").is_empty());
    std::fs::write(
        &manifest,
        "[dependencies]\ncpu = { path = '../../vendor/trust-build-cpu' }\n",
    )
    .unwrap();
    assert_eq!(
        path_dependants(root, "vendor/trust-sha2"),
        ["vendor/trust-build-cpu"]
    );
    assert_eq!(path_dependants(root, "vendor/trust-build-cpu"), ["member"]);
}
