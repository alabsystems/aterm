// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const DECLARED: &str = r##"
[harness]
id = "kimi-alab"
tool = "kimi-alab"
leads = ["kimi"]
contracts = [3, 4]
name = "Kimi Code"
env_strip = ["KIMI_", "MOONSHOT_"]

[harness.mark]
word = "kimi"
name = ["#38BDF8"]
icon = ["#38BDF8"]
texels = ["K ink 0", "k half 0"]
sd = ["K.", ".k"]
"##;

#[test]
fn a_declaration_reads_its_harness_table_and_mark() {
    let d = Declaration::from_file_text(DECLARED, true).expect("a well-formed declaration");
    assert_eq!(
        (
            d.id.as_str(),
            d.tool.as_str(),
            d.leads.as_slice(),
            d.name.as_str()
        ),
        (
            "kimi-alab",
            "kimi-alab",
            &["kimi".to_string()][..],
            "Kimi Code"
        )
    );
    assert_eq!(d.env_strip, ["KIMI_", "MOONSHOT_"]);
    assert_eq!(
        d.contracts,
        [3, 4],
        "the publisher accepts §2.1's `contracts`"
    );
    let mark = d.mark.expect("a mark");
    assert_eq!(mark.texels, [Texel::Ink(0), Texel::Half(0)]);
    assert_eq!(mark.name, [vec![0x38_BDF8], vec![0x38_BDF8]]);
}

#[test]
fn the_publisher_refuses_what_a_client_ignores() {
    let extra = DECLARED.replace("name = \"Kimi Code\"", "name = \"Kimi Code\"\nfuture = 1");
    assert!(Declaration::from_file_text(&extra, true).is_err());
    assert!(Declaration::from_file_text(&extra, false).is_ok());
}

#[test]
fn a_declaration_breaking_a_rule_is_refused() {
    for (from, to) in [
        ("id = \"kimi-alab\"", "id = \"kimi\""),
        ("leads = [\"kimi\"]", "leads = [\"Kimi\"]"),
        ("leads = [\"kimi\"]", "leads = [\"kimi\", \"kimi\"]"),
        ("\"MOONSHOT_\"", "\"PAT\""),
        ("\"MOONSHOT_\"", "\"LC_ALL\""),
        ("contracts = [3, 4]", "contracts = [0]"),
        ("contracts = [3, 4]", "contracts = [\"4\"]"),
        ("\"MOONSHOT_\"", "\"lower\""),
        ("sd = [\"K.\", \".k\"]", "sd = [\"K.\", \".x\"]"),
        (
            "texels = [\"K ink 0\", \"k half 0\"]",
            "texels = [\"K ink 1\", \"k half 0\"]",
        ),
        ("icon = [\"#38BDF8\"]", "icon = [\"38BDF8\"]"),
    ] {
        let text = DECLARED.replace(from, to);
        assert!(
            Declaration::from_file_text(&text, false).is_err(),
            "{from} -> {to} is refused"
        );
    }
}

#[test]
fn an_index_pins_one_leader_per_word() {
    let pin = |id: &str, leads: &[&str]| {
        (
            id.to_string(),
            leads.iter().map(|w| (*w).to_string()).collect::<Vec<_>>(),
        )
    };
    assert!(
        one_leader_per_word(&[pin("grok-alab", &["grok"]), pin("kimi-alab", &["kimi"])]).is_ok()
    );
    assert_eq!(
        one_leader_per_word(&[
            pin("kimi-alab", &["kimi"]),
            pin("grok-alab", &["grok", "kimi"])
        ]),
        Err("grok-alab and kimi-alab both lead `kimi`".to_string())
    );
    assert_eq!(
        pinned_leads("grok-alab", None),
        Some(vec!["grok".to_string()])
    );
    assert_eq!(pinned_leads("orca-alab", None), Some(Vec::new()));
    let table = harness_table_of(DECLARED).expect("a table");
    assert_eq!(
        pinned_leads("kimi-alab", Some(&table)),
        Some(vec!["kimi".to_string()])
    );
}

/// THE NAME TABLE JUDGES ONLY THE WORDS A ROW LEADS: each must be an agent the table reads, and
/// leading nothing is allowed, whatever the id and tool read as (agy-alab's and muse-alab's own
/// declarations lead nothing). NEGATIVE CONTROL: the rule before HX-05's review fix also required
/// the id or tool to read as the row's agent, and refused `quiet-alab` and each `zed-alab` here.
#[test]
fn the_name_table_judges_only_the_words_a_row_leads() {
    let read = |id: &str, leads: &str| {
        Declaration::from_file_text(
            &format!(
                "[harness]\nid = \"{id}\"\ntool = \"{id}\"\nleads = {leads}\nname = \"Test\"\n"
            ),
            true,
        )
        .expect("a well-formed declaration")
    };
    for (id, leads) in [
        ("agy-alab", "[]"),
        ("muse-alab", "[]"),
        ("kimi-alab", "[\"kimi\"]"),
        ("quiet-alab", "[]"),
        ("zed-alab", "[\"agy\"]"),
        ("zed-alab", "[\"kimi\", \"grok\"]"),
    ] {
        assert_eq!(
            name_table_refusal(&read(id, leads)),
            None,
            "{id} leading {leads}"
        );
    }
    let kimi = Declaration::from_file_text(DECLARED, true).expect("a well-formed declaration");
    assert_eq!(name_table_refusal(&kimi), None);
    for leads in ["[\"orca\"]", "[\"kimi\", \"orca\"]"] {
        let refusal = name_table_refusal(&read("zed-alab", leads)).expect("refused");
        assert!(refusal.contains("`orca`"), "{refusal}");
    }
}

/// A ready, complete build `store/<id>/<build>/bin/<id>` in `layout`, with the version record a
/// stage writes where `version` names one; its executable.
fn staged(
    layout: &crate::store::Layout,
    id: &str,
    build: u64,
    version: Option<&str>,
) -> std::path::PathBuf {
    let dir = layout.build_dir(id, build);
    std::fs::create_dir_all(dir.join("bin")).expect("build dir");
    let exe = dir.join("bin").join(id);
    std::fs::write(&exe, b"#!/bin/sh\n").expect("exe");
    crate::store::mark_build_ready(&dir).expect("ready");
    if let Some(version) = version {
        crate::store::write_version(&dir, version).expect("version record");
    }
    exe
}

/// AN IMAGE PROOF SAYS WHICH BUILD IT ADMITTED AND THAT BUILD'S RECORDED VERSION, on the two
/// arms no verified record stands behind: contract 3's rule for a seed id
/// ([`admits_seed_image`]) and a seed row's `AnyBuild` for a build that carries no record
/// ([`admits_image`]). The version is the one recorded beside that very build when it was
/// staged; a build beside it that records none is admitted with none.
/// NEGATIVE HALF: an id that is no seed id, and a file outside the store, admit nothing.
#[test]
fn a_seed_build_with_no_record_is_admitted_under_its_recorded_version() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let layout = crate::store::Layout {
        prefix: root.join("pkg"),
    };
    let versioned = staged(&layout, "grok-alab", 412, Some("0.4.0+g1a2b3c"));
    let bare = staged(&layout, "grok-alab", 413, None);
    let build = |build: u64, version: Option<&str>| {
        Some(Admitted::Build {
            build,
            version: version.map(str::to_string),
        })
    };

    assert_eq!(
        admits_seed_image(&layout, "grok-alab", &versioned),
        build(412, Some("0.4.0+g1a2b3c")),
        "contract 3: the build's own recorded version"
    );
    assert_eq!(
        admits_seed_image(&layout, "grok-alab", &bare),
        build(413, None),
        "contract 3: a build that records none, not its neighbour's"
    );

    let trust = Trust::of_layout(&layout);
    let composed = compose(&layout, &crate::config::HarnessConfig::default(), &trust);
    let row = composed
        .rows
        .iter()
        .find(|row| row.declaration.id == "grok-alab")
        .expect("the seed row");
    assert_eq!(row.origin, Origin::Seed);
    assert!(row.images.contains(&Image::AnyBuild), "{:?}", row.images);
    assert_eq!(
        admits_image(&layout, row, &versioned, &trust),
        build(412, Some("0.4.0+g1a2b3c")),
        "a seed row's build with no record: its recorded version"
    );
    assert_eq!(
        admits_image(&layout, row, &bare, &trust),
        build(413, None),
        "a seed row's build that records no version"
    );

    // NEGATIVE HALF.
    let other = staged(&layout, "other-alab", 5, Some("9.9.0"));
    assert_eq!(admits_seed_image(&layout, "other-alab", &other), None);
    assert_eq!(admits_image(&layout, row, &other, &trust), None);
    let stray = root.join("grok-alab");
    std::fs::write(&stray, b"x").expect("stray");
    assert_eq!(admits_seed_image(&layout, "grok-alab", &stray), None);
    assert_eq!(admits_image(&layout, row, &stray, &trust), None);
}
