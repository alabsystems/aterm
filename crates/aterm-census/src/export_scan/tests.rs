// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::*;
use crate::scan_set::test_fixtures::disk_tree;

const BASELINE: &str = "# Paths\nquokkaword\n";

fn engine() -> Engine {
    Engine {
        baseline: ("synthetic baseline".into(), BASELINE.into()),
        names: vec!["aterm".into(), "ay".into()],
    }
}

/// The org with each letter's case flipped where `upper` says, which the export's transforms
/// (exact lower case) leave and the engine's org rewrite reads.
fn org_cased(upper: &[usize]) -> String {
    PRIVATE_ORG
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if upper.contains(&i) {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect()
}

fn tree(name: &str, files: &[(&str, String)]) -> PathBuf {
    let owned: Vec<(String, String)> = files
        .iter()
        .map(|(rel, text)| ((*rel).to_string(), text.clone()))
        .collect();
    disk_tree("aterm-census-export-scan", name, &owned)
}

fn listed(files: &[(&str, String)]) -> Vec<String> {
    files.iter().map(|(rel, _)| (*rel).to_string()).collect()
}

/// `files` under `root` scanned with the synthetic engine, `whole` or as a cone.
fn scanned(root: &Path, files: &[(&str, String)], whole: bool, into: Option<&Path>) -> Scanned {
    let paths = listed(files);
    let listing = Listing {
        root,
        paths: &paths,
        gitlinks: &[],
        whole,
        export: &|| paths.clone(),
    };
    scan(&listing, Some(&engine()), into)
}

fn spots(found: &[Finding]) -> BTreeSet<(Guard, String, usize)> {
    found
        .iter()
        .map(|f| (f.guard, f.path.clone(), f.line))
        .collect()
}

#[test]
fn a_private_reference_the_org_rewrite_leaves_is_found_at_its_line() {
    let cased = org_cased(&[0, 6]);
    let files = [
        (MANIFEST, "crates\n".to_string()),
        (
            "crates/x/src/lib.rs",
            format!("// github.com/{cased}/aterm\n// {cased}/other\n"),
        ),
        (
            "crates/x/Cargo.toml",
            format!("[dependencies]\nz = {{ git = \"https://github.com/{cased}/ay\" }}\n"),
        ),
        ("crates/x/Cargo.lock", format!("\n\n{cased}/ay\n")),
        ("tools/outside.md", format!("{cased}/other\n")),
    ];
    let root = tree("private", &files);
    let found = scanned(&root, &files, false, None).findings;
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        spots(&found),
        [
            (Guard::PrivateRefs, "crates/x/src/lib.rs", 2),
            (Guard::PrivateRefs, "crates/x/Cargo.toml", 2),
            (Guard::PrivateRefs, "crates/x/Cargo.lock", 3),
        ]
        .map(|(g, p, l)| (g, p.to_string(), l))
        .into(),
        "the registry repo is rewritten, every other spelling stands, and outside the \
         manifest nothing is judged: {found:#?}"
    );
    assert!(
        found.iter().all(|f| !f.detail.contains(&cased)),
        "a finding never spells the org: {found:#?}"
    );
}

#[test]
fn a_private_reference_in_any_wide_encoding_is_found() {
    let text = format!("a\n{PRIVATE_ORG}\n");
    let utf16 = |little: bool| -> Vec<u8> {
        text.encode_utf16()
            .flat_map(|u| {
                if little {
                    u.to_le_bytes()
                } else {
                    u.to_be_bytes()
                }
            })
            .collect()
    };
    let utf32 = |little: bool| -> Vec<u8> {
        text.chars()
            .flat_map(|c| {
                let u = u32::from(c);
                if little {
                    u.to_le_bytes()
                } else {
                    u.to_be_bytes()
                }
            })
            .collect()
    };
    for (how, bytes) in [
        ("UTF-16LE", utf16(true)),
        ("UTF-16BE", utf16(false)),
        ("UTF-32LE", utf32(true)),
        ("UTF-32BE", utf32(false)),
    ] {
        let mut found = Vec::new();
        private_refs("x.txt", &bytes, &mut found);
        assert!(
            found
                .iter()
                .any(|f| f.line == 2 && f.detail.contains(&format!(" in {how},"))),
            "{how}: {found:#?}"
        );
    }
    let mut clean = Vec::new();
    private_refs("x.txt", b"no org here\n", &mut clean);
    assert!(clean.is_empty(), "{clean:#?}");
}

#[test]
fn the_export_is_written_as_exported_for_gitleaks() {
    let home = concat!("/Us", "ers/someone/x");
    let files = [
        (MANIFEST, "crates\n.gitleaks.toml\n".to_string()),
        (
            ".gitleaks.toml",
            "[extend]\nuseDefault = true\n".to_string(),
        ),
        ("crates/x/src/lib.rs", format!("// {home}\n")),
        ("crates/x/src/same.rs", "// plain\n".to_string()),
        ("docs/withheld.md", "// plain\n".to_string()),
    ];
    let root = tree("written", &files);
    let into = root.with_extension("exported");
    let _ = std::fs::remove_dir_all(&into);
    let scanned = scanned(&root, &files, true, Some(&into));
    let read = |rel: &str| std::fs::read_to_string(into.join(rel)).ok();
    let (lib, same, toml, withheld) = (
        read("crates/x/src/lib.rs"),
        read("crates/x/src/same.rs"),
        read(".gitleaks.toml"),
        read("docs/withheld.md"),
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&into);
    assert!(scanned.findings.is_empty(), "{:#?}", scanned.findings);
    assert_eq!(scanned.files, 3);
    assert_eq!(
        lib.as_deref(),
        Some("// /Users//someone/x\n"),
        "the transform ran"
    );
    assert_eq!(same.as_deref(), Some("// plain\n"));
    assert!(toml.is_some(), "the config gitleaks reads is exported");
    assert_eq!(withheld, None, "what the manifest withholds is not written");
}

#[test]
fn astreams_files_stand_in_for_the_gitlink_the_manifest_withholds() {
    let selection = Selection::parse("vendor\n!vendor/astream\n").expect("parses");
    assert!(selects(&selection, "vendor/astream/crates/w/src/lib.rs"));
    assert!(selects(&selection, "vendor/astream/Cargo.toml"));
    assert!(!selects(&selection, "vendor/astream/crates/w/tests/t.rs"));
    assert!(!selects(&selection, "vendor/astream/README.md"));
    assert!(!selects(&selection, "vendor/astream"));
    assert!(selects(&selection, "vendor/other/a.rs"));
    assert!(
        !selects(&Selection::everything(), ".gitmodules"),
        "the transforms delete it"
    );
}

#[test]
fn a_manifest_selecting_nothing_is_a_finding_only_over_the_whole_checkout() {
    let files = [
        (MANIFEST, "toolz\n".to_string()),
        ("crates/x/src/lib.rs", "// x\n".to_string()),
    ];
    let root = tree("selects-none", &files);
    let whole = scanned(&root, &files, true, None).findings;
    let cone = scanned(&root, &files, false, None).findings;
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        spots(&whole),
        [(Guard::Manifest, MANIFEST.to_string(), 0)].into(),
        "{whole:#?}"
    );
    assert!(
        cone.is_empty(),
        "a change the export does not carry: {cone:#?}"
    );
}

#[test]
fn a_gitlink_the_manifest_selects_is_a_finding() {
    let files = [
        (MANIFEST, "crates\n!crates/held\n".to_string()),
        ("crates/x/src/lib.rs", "// x\n".to_string()),
    ];
    let root = tree("gitlinks", &files);
    let paths = listed(&files);
    let gitlinks = [
        "crates/sub".to_string(),
        "crates/held".to_string(),
        ASTREAM.to_string(),
    ];
    let listing = Listing {
        root: &root,
        paths: &paths,
        gitlinks: &gitlinks,
        whole: true,
        export: &|| paths.clone(),
    };
    let found = scan(&listing, Some(&engine()), None).findings;
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        found
            .iter()
            .map(|f| (f.guard, f.detail.contains("crates/sub")))
            .collect::<Vec<_>>(),
        [(Guard::Manifest, true)],
        "only the gitlink the manifest selects: {found:#?}"
    );
}

#[test]
fn without_the_engine_the_exports_own_rows_still_hold() {
    let lib = "crates/zebraname/src/lib.rs";
    let cased = org_cased(&[0, 6]);
    let files = [
        (MANIFEST, "crates\n".to_string()),
        (EXTRA, "zebraword\n^zebraname$\n".to_string()),
        (
            lib,
            format!("// zebraword\n// quokkaword\n// {cased}/other\n"),
        ),
    ];
    let root = tree("offline", &files);
    let paths = listed(&files);
    let listing = Listing {
        root: &root,
        paths: &paths,
        gitlinks: &[],
        whole: false,
        export: &|| paths.clone(),
    };
    let offline = scan(&listing, None, None).findings;
    let online = scan(&listing, Some(&engine()), None).findings;
    let _ = std::fs::remove_dir_all(&root);
    let at = |spots: &[(Guard, usize)]| -> BTreeSet<(Guard, String, usize)> {
        spots
            .iter()
            .map(|&(g, l)| (g, lib.to_string(), l))
            .collect()
    };
    assert_eq!(
        spots(&offline),
        at(&[(Guard::Forbidden, 0), (Guard::Forbidden, 1)]),
        "the export's own rows, a path row matching one component: {offline:#?}"
    );
    assert_eq!(
        spots(&online),
        at(&[
            (Guard::Forbidden, 0),
            (Guard::Forbidden, 1),
            (Guard::Forbidden, 2),
            (Guard::PrivateRefs, 3),
        ]),
        "with the engine, its baseline and the private org too: {online:#?}"
    );
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_is_exported_as_one() {
    let files = [
        (MANIFEST, "crates\n".to_string()),
        ("crates/x/a.rs", "// a\n".to_string()),
    ];
    let root = tree("link", &files);
    std::os::unix::fs::symlink("a.rs", root.join("crates/x/b.rs")).expect("link");
    let into = root.with_extension("linked");
    let _ = std::fs::remove_dir_all(&into);
    let paths = vec!["crates/x/a.rs".to_string(), "crates/x/b.rs".to_string()];
    let listing = Listing {
        root: &root,
        paths: &paths,
        gitlinks: &[],
        whole: false,
        export: &|| paths.clone(),
    };
    let scanned = scan(&listing, Some(&engine()), Some(&into));
    let target = std::fs::read_link(into.join("crates/x/b.rs"));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&into);
    assert!(scanned.findings.is_empty(), "{:#?}", scanned.findings);
    assert_eq!(target.ok(), Some(PathBuf::from("a.rs")));
}
