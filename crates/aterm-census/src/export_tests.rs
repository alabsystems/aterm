// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn the_rewrites_take_exactly_the_guards_shapes() {
    let acct = ACCOUNT;
    let case = |input: String, want: &str| {
        assert_eq!(
            String::from_utf8(rewrite(input.as_bytes())).unwrap(),
            want,
            "{input}"
        );
    };
    case(
        format!("https://github.com/{PRIVATE_ORG}/aterm"),
        "https://github.com/alabsystems/aterm",
    );
    case(
        format!("/Users/{acct}/aterm/target"),
        "/Users//example/aterm/target",
    );
    case("/Users//someone/Library".into(), "/Users//someone/Library");
    case(format!("{TILDE_TRUST}/build"), "$HOME/trust/build");
    case(format!("{TILDE_CLEAN}/bin"), "$HOME/clean/bin");
    case(format!("b\"{acct}@mac ~ % \""), "b\"user@mac ~ % \"");
    case(
        "path.starts_with(\"/Users/\")".into(),
        "path.starts_with(\"/Users/\")",
    );
    case("version = \"0.104.7\"".into(), "version = \"0.104.7\"");
}

#[test]
fn the_selection_reads_the_manifest_as_git_reads_pathspecs() {
    let s = Selection::parse(
        "# head\nREADME.md\ncrates # the crates\ndocs\n!crates/x/proofs\n\
         !crates/x/perf-*.tsv\n!crates/x/s2_??.toml\n!vendor/*/.github\nvendor\n",
    )
    .expect("parses");
    for (rel, want) in [
        ("README.md", true),
        ("README.mdx", false),
        ("crates/x/src/lib.rs", true),
        ("cratesx/lib.rs", false),
        ("docs/a.md", false),
        ("crates/x/proofs/a.json", false),
        ("crates/x/proofsx/a.json", true),
        ("crates/x/perf-a.tsv", false),
        ("crates/x/sub/perf-a.tsv", true),
        ("crates/x/s2_01.toml", false),
        ("crates/x/s2_011.toml", true),
        ("vendor/y/.github", false),
        ("vendor/y/.github/w.yml", true),
        ("publish/manifest.txt", false),
        ("tools/install.sh", false),
    ] {
        assert_eq!(s.selects(rel), want, "{rel}");
    }
    assert!(Selection::everything().selects("tools/install.sh"));
    assert!(!Selection::everything().selects(".github/workflows/a.yml"));
    for root in [".", "././", "././."] {
        let selected = Selection::parse(&format!("{root}\n!./crates/x/proofs/\n"))
            .expect("root include with a directory exclusion");
        for (rel, want) in [
            ("README.md", true),
            (".root", true),
            ("tools/install.sh", true),
            ("crates/x/src/lib.rs", true),
            ("crates/x/proofs/a.json", false),
            ("docs/a.md", false),
            ("publish/manifest.txt", false),
            (".github/workflows/a.yml", false),
        ] {
            assert_eq!(selected.selects(rel), want, "{root}: {rel}");
        }
    }
    for root in [".", "././", "././."] {
        let excluded = Selection::parse(&format!("*\n!{root}\n")).expect("root exclusion");
        for rel in ["README.md", ".root", "crates/x/src/lib.rs"] {
            assert!(!excluded.selects(rel), "{root}: {rel}");
        }
    }
    assert_eq!(Selection::parse("/\n").unwrap_err().0, 1);
    assert_eq!(
        Selection::parse("crates\n!crates/[ab]\n")
            .map(|_| ())
            .unwrap_err()
            .0,
        2
    );
}

/// What `git ls-files -- <spec>` (git 2.54.0) lists of a checkout holding
/// `README.md`, `crates/x/a.txt` and `crates/xy/b.txt`, measured once per
/// spec; an exclusion is `git ls-files -- . ':(exclude)<spec>'`.
#[test]
fn a_spelling_git_normalizes_selects_what_git_lists() {
    const FILES: [&str; 3] = ["README.md", "crates/x/a.txt", "crates/xy/b.txt"];
    let (readme, x, xy) = (&FILES[..1], &FILES[1..2], &FILES[2..]);
    for (spec, listed) in [
        ("crates//x", x),
        ("crates/./x", x),
        ("crates/x/.", x),
        ("crates/x//", x),
        ("./crates/./x", x),
        ("crates/x/../xy", xy),
        ("crates/x/..", &FILES[1..]),
        ("crates//*.txt", &FILES[1..]),
        ("crates/..", &FILES[..]),
        ("cr*/../README.md", readme),
        ("crates/x/../../README.md", readme),
        ("././README.md", readme),
        ("README.md/", &[]),
        ("README.md/.", &[]),
        ("crates/x/a.txt/", &[]),
        ("crates/x/..//README.md", &[]),
        ("crates/*/", &[]),
        ("cr*/", &[]),
    ] {
        let included = Selection::parse(spec).expect(spec);
        let excluded = Selection::parse(&format!(".\n!{spec}\n")).expect(spec);
        for rel in FILES {
            let want = listed.contains(&rel);
            assert_eq!(included.selects(rel), want, "{spec}: {rel}");
            assert_eq!(excluded.selects(rel), !want, "!{spec}: {rel}");
        }
    }
}

#[test]
fn a_line_the_engine_refuses_is_refused() {
    for (manifest, line) in [
        ("crates\n!\n", 2),
        ("crates\n! \n", 2),
        ("crates\n!./\n", 2),
        ("crates\n! ./ # the root\n", 2),
        ("./\n", 1),
        (".//\n", 1),
        ("..\n", 1),
        ("../x\n", 1),
        ("crates/../..\n", 1),
        ("crates\n!crates/x/../../..\n", 2),
    ] {
        assert_eq!(
            Selection::parse(manifest).map(|_| ()).unwrap_err().0,
            line,
            "{manifest:?}"
        );
    }
    for manifest in [
        "crates\n!.\n",
        "crates\n!././\n",
        "././docs\n",
        "docsx\n",
        "",
    ] {
        assert!(Selection::parse(manifest).is_ok(), "{manifest:?}");
    }
}
