// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn independent_wasm_roots_each_need_a_wasm_cell() {
    let roots = ["crates/cpu", "crates/gpu"];
    let mut cells = vec![
        Cell {
            name: "cpu".into(),
            triple: "wasm32-unknown-unknown".into(),
            package: "cpu".into(),
        },
        Cell {
            name: "gpu".into(),
            triple: "wasm32-unknown-unknown".into(),
            package: "gpu".into(),
        },
    ];
    assert!(unmeasured_wasm_roots(&cells, &roots).is_empty());
    cells.remove(0);
    assert_eq!(unmeasured_wasm_roots(&cells, &roots), ["crates/cpu"]);
    cells.push(Cell {
        name: "native-cpu".into(),
        triple: "aarch64-apple-darwin".into(),
        package: "cpu".into(),
    });
    assert_eq!(unmeasured_wasm_roots(&cells, &roots), ["crates/cpu"]);
}

#[test]
fn tree_lines_parse_into_depth_id_path_and_annotations() {
    type Want = (
        usize,
        &'static str,
        &'static str,
        Option<&'static str>,
        bool,
        bool,
    );
    let cases: [(&str, Want); 7] = [
        (
            "8unicode-ident v1.0.24",
            (8, "unicode-ident", "1.0.24", None, false, false),
        ),
        (
            "0aterm v0.47.0 (/Users//a/aterm/crates/aterm)",
            (
                0,
                "aterm",
                "0.47.0",
                Some("/Users//a/aterm/crates/aterm"),
                false,
                false,
            ),
        ),
        (
            "6aterm-error-derive v0.47.0 (proc-macro) (/w/crates/aterm-error-derive)",
            (
                6,
                "aterm-error-derive",
                "0.47.0",
                Some("/w/crates/aterm-error-derive"),
                true,
                false,
            ),
        ),
        (
            "3serde v1.0.228 (*)",
            (3, "serde", "1.0.228", None, false, true),
        ),
        (
            "4indexmap v2.14.0 (/Users//a/aterm/vendor/indexmap) (*)",
            (
                4,
                "indexmap",
                "2.14.0",
                Some("/Users//a/aterm/vendor/indexmap"),
                false,
                true,
            ),
        ),
        (
            "2foo v1.0.0 (/Users//a b (1)/vendor/foo) (*)",
            (
                2,
                "foo",
                "1.0.0",
                Some("/Users//a b (1)/vendor/foo"),
                false,
                true,
            ),
        ),
        (
            "4zstd-sys v2.0.16+zstd.1.5.7",
            (4, "zstd-sys", "2.0.16+zstd.1.5.7", None, false, false),
        ),
    ];
    for (raw, (depth, name, version, path, pm, deduped)) in cases {
        let line = parse_line(raw).expect(raw).expect(raw);
        assert_eq!(line.depth, depth, "{raw}");
        assert_eq!(line.id, PkgId::new(name, version), "{raw}");
        assert_eq!(line.path.as_deref(), path.map(Path::new), "{raw}");
        assert_eq!((line.is_proc_macro, line.deduped), (pm, deduped), "{raw}");
    }
    assert_eq!(parse_line("   ").unwrap(), None, "a blank line is skipped");
}

#[test]
fn an_unreadable_tree_line_refuses() {
    for raw in ["|-- serde v1.0.228", "2serde", "2serde 1.0.228"] {
        assert!(parse_line(raw).is_err(), "{raw}");
    }
    for tree in ["0root v0.1.0\n2orphan v1.0.0\n", "1alpha v1.0.0\n"] {
        assert!(parse_tree(tree).is_err(), "{tree}");
    }
}

#[cfg(unix)]
#[test]
fn an_offline_miss_retries_online_once_and_notes_it() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("tree"),
        "while [ $# -gt 0 ]; do\n\
         case $1 in --offline) exit 101 ;; -p) pkg=$2 ;; esac\n\
         shift\n\
         done\n\
         echo \"0$pkg v0.1.0\"\n",
    )
    .expect("fake tree");
    let cell = Cell {
        name: "mac-arm".into(),
        triple: "aarch64-apple-darwin".into(),
        package: "aterm".into(),
    };
    let mut log = String::new();
    let (graph, _) = graph_and_paths(
        &Cargo::pinned("/bin/sh", "/usr/bin:/bin"),
        dir.path(),
        &cell,
        &mut log,
    )
    .expect("the online retry resolves the cell");
    assert_eq!(graph.root, PkgId::new("aterm", "0.1.0"));
    assert!(!log.is_empty(), "the online retry must be noted");
}

const FIXTURE: &str = "\
0root v0.1.0 (/w/crates/root)
1alpha v1.0.0
2shared v9.9.9
1beta v2.0.0 (proc-macro)
2shared v9.9.9 (*)

2gamma v3.0.0 (/w/vendor/gamma)
";

#[test]
fn the_depth_stack_reconstructs_parents() {
    let (g, paths) = parse_tree(FIXTURE).unwrap();
    assert_eq!(g.root, PkgId::new("root", "0.1.0"));
    assert_eq!(g.nodes.len(), 5, "root, alpha, beta, shared, gamma");
    let kids = |n: &str, v: &str| {
        g.edges
            .get(&PkgId::new(n, v))
            .map(|s| s.iter().map(ToString::to_string).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    assert_eq!(kids("root", "0.1.0"), ["alpha 1.0.0", "beta 2.0.0"]);
    assert_eq!(kids("alpha", "1.0.0"), ["shared 9.9.9"]);
    assert_eq!(kids("beta", "2.0.0"), ["gamma 3.0.0", "shared 9.9.9"]);
    assert_eq!(
        paths[&PkgId::new("gamma", "3.0.0")],
        PathBuf::from("/w/vendor/gamma")
    );
    assert!(!paths.contains_key(&PkgId::new("alpha", "1.0.0")));
}

#[test]
fn git_annotations_are_bound_to_resolved_full_commits() {
    let full = "1234567890123456789012345678901234567890";
    let source = format!("git+https://github.com/team/core?rev={full}#{full}");
    assert!(matching_git_annotation(
        Path::new(&format!("https://github.com/team/core?rev={full}#1234567")),
        &source,
    ));
    for annotation in [
        format!("https://github.com/other/core?rev={full}#1234567"),
        format!("https://github.com/team/core?rev={full}#9876543"),
        format!("https://github.com/team/core?rev={full}#"),
    ] {
        assert!(!matching_git_annotation(Path::new(&annotation), &source));
    }
}

#[test]
fn git_metadata_resolves_directories_and_refuses_missing_or_conflicting_identity() {
    let full = "1234567890123456789012345678901234567890";
    let row = |path: &str, hash: &str| {
        format!(
            r#"{{"name":"leaf","version":"1.0.0","source":"git+https://github.com/team/core?rev={hash}#{hash}","manifest_path":"{path}"}}"#
        )
    };
    let absolute = if cfg!(windows) {
        "C:/source/leaf/Cargo.toml"
    } else {
        "/source/leaf/Cargo.toml"
    };
    let valid = row(absolute, full);
    let packages = parse_git_packages(&format!(r#"{{"packages":[{valid}]}}"#)).unwrap();
    assert_eq!(
        packages[&PkgId::new("leaf", "1.0.0")].directory,
        Path::new(absolute).parent().unwrap()
    );
    assert!(
        parse_git_packages(&format!(
            r#"{{"packages":[{}]}}"#,
            row("relative/Cargo.toml", full)
        ))
        .is_err()
    );
    assert!(
        parse_git_packages(&format!(
            r#"{{"packages":[{valid},{}]}}"#,
            row(absolute, "9876543210987654321098765432109876543210")
        ))
        .is_err()
    );
    let local = format!(
        r#"{{"name":"leaf","version":"1.0.0","source":null,"manifest_path":"{absolute}"}}"#
    );
    assert!(parse_git_packages(&format!(r#"{{"packages":[{valid},{local}]}}"#)).is_err());
    assert!(parse_git_packages("{}").is_err());
}
