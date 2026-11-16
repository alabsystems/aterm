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
