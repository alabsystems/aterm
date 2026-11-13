// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const WINDOW: &str = concat!(
    r#"models:[{id:"claude-opus-5",family:"opus",display_name:"Opus 5",knowledge_cutoff:"May 2026","#,
    r#"context:{window:2e5,native_1m:!1,supports_1m_beta:!0},capabilities:["effort","max_effort","adaptive_thinking"],default_effort:"high"},"#,
    r#"{id:"claude-opus-5-5",family:"opus",display_name:"Opus 5.5",knowledge_cutoff:"June 2026","#,
    r#"context:{window:1e6,native_1m:!0,supports_1m_beta:!0,supports_1m_suffix:!0},capabilities:["effort","max_effort","xhigh_effort","adaptive_thinking"],default_effort:"medium",advisor_rank:4}],"#,
    r#"aliases:{opus:{default:"claude-opus-5-5"}},defaults:{},best:"fable","#,
    r#"latest_per_family:{fable:"claude-fable-5-1",opus:"claude-opus-5-5",sonnet:"claude-sonnet-5",haiku:"claude-haiku-4-5"},alias_migration:{}};"#,
);

#[test]
fn the_280_literal_parses() {
    let b = parse_window(WINDOW).expect("parses");
    assert_eq!(b.latest("opus"), Some("claude-opus-5-5"));
    assert_eq!(b.latest("fable"), Some("claude-fable-5-1"));
    assert_eq!(b.latest("mythos"), None);
    let o55 = b.model("claude-opus-5-5").expect("entry");
    assert_eq!(o55.family, "opus");
    assert_eq!(o55.display_name, "Opus 5.5");
    assert!(o55.native_1m);
    assert_eq!(o55.efforts, ["low", "medium", "high", "xhigh", "max"]);
    let o5 = b.model("claude-opus-5[1m]").expect("the suffix is ignored");
    assert!(!o5.native_1m);
    assert!(!o5.efforts.contains(&"xhigh".to_string()));
}

#[test]
fn a_hostile_value_is_not_a_model() {
    let w = WINDOW.replace(
        r#"opus:"claude-opus-5-5",sonnet"#,
        r#"opus:"claude-opus-5-5;rm -rf ~",sonnet"#,
    );
    assert_ne!(w, WINDOW, "the fixture must contain the replaced text");
    let b = parse_window(&w).expect("the rest still parses");
    assert_eq!(b.latest("opus"), None);
    assert_eq!(parse_window("no marker here"), None);
}

#[test]
fn build_facts_round_trip_through_the_disk_cache() {
    let b = BuildFacts {
        baked: parse_window(WINDOW),
        version: Some("2.1.280".into()),
    };
    assert_eq!(parse_facts(&render_facts(&b)), Some(b));
    assert_eq!(parse_facts("{}"), Some(BuildFacts::default()));
    for bad in ["02.1.280", "2.01.280", "2.1.0280", "0.0.00"] {
        assert!(!is_version(bad), "{bad}");
        assert_eq!(version_triple(bad), None, "{bad}");
        assert_eq!(
            parse_facts(&format!(r#"{{"version":"{bad}"}}"#)).and_then(|facts| facts.version),
            None,
            "{bad}"
        );
    }
    assert_eq!(version_triple("0.0.0"), Some((0, 0, 0)));
}

#[test]
fn a_cached_catalog_is_found_by_the_version_its_build_says() {
    let dir = crate::supervise::ScratchDir::new("upgrade-catalog", "by-version");
    let facts = |version: &str, id: &str| BuildFacts {
        version: Some(version.to_string()),
        baked: Some(Baked {
            latest_per_family: vec![("opus".to_string(), id.to_string())],
            models: vec![BakedModel {
                id: id.to_string(),
                family: "opus".to_string(),
                display_name: "Opus".to_string(),
                native_1m: false,
                efforts: vec!["high".to_string(), "max".to_string()],
            }],
        }),
    };
    for (name, f) in [
        ("a.json", facts("2.1.300", "claude-opus-5")),
        ("b.json", facts("2.1.321", "claude-opus-5-5")),
    ] {
        std::fs::write(dir.join(name), render_facts(&f)).expect("write");
    }
    std::fs::write(dir.join("c.json"), "not json").expect("write");
    let found = cached_for_version(&dir, "2.1.321").expect("the build's catalog");
    assert_eq!(found.latest("opus"), Some("claude-opus-5-5"));
    assert_eq!(cached_for_version(&dir, "2.1.999"), None);
    assert_eq!(cached_for_version(&dir.join("absent"), "2.1.321"), None);
}

#[test]
fn the_scan_finds_a_marker_across_a_chunk_boundary() {
    const BOUNDARY_SINGLE: &[u8] = b"VERSION:\"9.9.9\"";
    let dir = crate::supervise::ScratchDir::new("upgrade-catalog", "boundary");
    let path = dir.join("claude");
    let mut bytes = vec![b'x'; CHUNK - 15 - BOUNDARY_SINGLE.len()];
    bytes.extend_from_slice(BOUNDARY_SINGLE);
    bytes.extend_from_slice(WINDOW.as_bytes());
    bytes.extend(std::iter::repeat_n(b'y', 1000));
    bytes.extend_from_slice(b"VERSION:\"2.1.281\" VERSION:\"2.1.281\"");
    std::fs::write(&path, &bytes).expect("write");
    let f = scan_build(&path);
    let b = f.baked.expect("found");
    assert_eq!(b.latest("opus"), Some("claude-opus-5-5"));
    assert_eq!(b.models.len(), 2);
    assert_eq!(
        f.version.as_deref(),
        Some("2.1.281"),
        "the most frequent literal wins without double-counting carry"
    );

    let mut near_end = vec![b'x'; CHUNK - WINDOW.len()];
    near_end.extend_from_slice(WINDOW.as_bytes());
    assert_eq!(near_end.len(), CHUNK);
    near_end.extend_from_slice(b"VERSION:\"2.1.281\" VERSION:\"2.1.281\"");
    std::fs::write(&path, &near_end).expect("write");
    let f2 = scan_build(&path);
    assert_eq!(
        f2.baked.as_ref().and_then(|b| b.latest("opus")),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        f2.version.as_deref(),
        Some("2.1.281"),
        "a marker near chunk end still scans version literals in the lookahead window"
    );
}

#[test]
fn the_newest_of_a_family_is_the_entry_the_catalog_names_for_it() {
    let baked = parse_window(WINDOW).expect("catalog");
    let opus = baked.latest_of("opus").expect("the newest opus");
    assert_eq!(
        (opus.id.as_str(), opus.display_name.as_str()),
        ("claude-opus-5-5", "Opus 5.5")
    );
    assert_eq!(
        baked.latest_of("fable"),
        None,
        "a family the build names no newest of"
    );
    let unlisted = Baked {
        latest_per_family: vec![("fable".to_string(), "claude-fable-5-1".to_string())],
        models: vec![],
    };
    assert_eq!(
        unlisted.latest_of("fable"),
        None,
        "a newest the catalog lists no entry for"
    );
}

#[test]
fn a_session_s_catalog_is_its_build_s_cached_one_and_none_where_unswept() {
    let root = crate::supervise::ScratchDir::new("upgrade-catalog", "session-build");
    let state: &std::path::Path = &root;
    assert_eq!(catalog(state, Some("2.1.281")), None, "no upgrade tree");
    let dir = super::super::upgrade_drive::ensure_upgrade_tree(state, Some("builds"))
        .expect("builds dir");
    let facts = BuildFacts {
        baked: parse_window(WINDOW),
        version: Some("2.1.281".to_string()),
    };
    std::fs::write(dir.join("0000000000000001.json"), render_facts(&facts)).expect("write");
    assert_eq!(catalog(state, Some("2.1.281")), facts.baked);
    assert_eq!(
        catalog(state, Some("2.1.999")),
        None,
        "a build no sweep cached"
    );
    assert_eq!(
        catalog(state, None),
        None,
        "a session whose build is not known"
    );
}
