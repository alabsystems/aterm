// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SAMPLE: &str = r#"# SPDX-License-Identifier: Apache-2.0
# a comment block that IS the record

[forge]
loc_method = "rs-physical-all-files-v1"
graph_method = "cargo-tree-normal-no-dedupe-locked-offline-v1"

# --- indexmap 2.14.0 ---------------------------------------------------------
# Vendored patch fixture. THIS PARAGRAPH is why the fork exists, and a round-trip
# that eats it defeats the file.
[[fork]]
name = "indexmap"
version = "2.14.0"
path = "vendor/indexmap"
license = "Apache-2.0"
apache_notice = true
census.mode = "scanned"
census.namespace = "indexmap"

# smol_str is dual-licensed, so it owes no §4(b) notices.
[[fork]]
name = "smol_str"
version = "0.2.2"
path = "vendor/smol_str"
license = "MIT OR Apache-2.0"
census.mode = "scanned"
census.namespace = "smol_str"
"#;

#[test]
fn forks_are_modeled_including_census_and_notice() {
    let p = parse(SAMPLE).unwrap();
    assert_eq!(p.forks.len(), 2);
    let idx = p
        .forks
        .iter()
        .find(|f| f.name == "indexmap")
        .expect("indexmap block");
    assert_eq!(idx.version, "2.14.0");
    assert_eq!(idx.license, "Apache-2.0");
    assert!(idx.apache_notice);
    assert_eq!(idx.census_mode, CensusMode::Scanned);
    assert_eq!(idx.census_namespace.as_deref(), Some("indexmap"));
    let ss = p
        .forks
        .iter()
        .find(|f| f.name == "smol_str")
        .expect("smol_str block");
    assert!(!ss.apache_notice, "apache_notice defaults to false");
    assert_eq!(ss.census_namespace.as_deref(), Some("smol_str"));
}

#[test]
fn a_ledger_that_does_not_parse_cleanly_is_refused() {
    let dup = format!(
        "{SAMPLE}\n[[fork]]\nname = \"indexmap\"\nversion = \"2.14.0\"\n\
         path = \"vendor/indexmap\"\nlicense = \"Apache-2.0\"\ncensus.mode = \"scanned\"\n\
         census.namespace = \"indexmap\"\n"
    );
    let headerless = format!(
        "# --- indexmap{}",
        SAMPLE.split("# --- indexmap").nth(1).unwrap()
    );
    for (name, text) in [
        (
            "an unknown key",
            SAMPLE.replace("apache_notice = true", "apache_notices = true"),
        ),
        (
            "an unknown top-level table",
            format!("{SAMPLE}\n[carve]\nwhen = \"soon\"\n"),
        ),
        (
            "a scanned fork without a namespace",
            SAMPLE.replace("census.namespace = \"indexmap\"\n", ""),
        ),
        (
            "a retired census mode",
            SAMPLE.replace(
                "census.mode = \"scanned\"\ncensus.namespace = \"smol_str\"",
                "census.mode = \"build-dep-only\"",
            ),
        ),
        (
            "a drifted method string",
            SAMPLE.replace(LOC_METHOD, "rs-logical-v2"),
        ),
        ("no [forge] header", headerless),
        ("two blocks for one package", dup),
    ] {
        assert_ne!(text, SAMPLE, "{name}: the plant must change the sample");
        assert!(parse(&text).is_err(), "{name}");
    }
}

#[test]
fn a_semver_class_is_the_version_line_cargo_resolves_within() {
    for (v, class) in [
        ("1.4.2", "1"),
        ("^1.4", "1"),
        ("0", "0"),
        ("0.0", "0.0"),
        ("0.52.0", "0.52"),
        ("0.52", "0.52"),
        ("=0.52.0", "0.52"),
        ("0.61", "0.61"),
        ("0.0.3", "0.0.3"),
        ("2.0.16+zstd.1.5.7", "2"),
        ("0.3.0-alpha.1", "0.3"),
    ] {
        assert_eq!(semver_class(v), class, "{v}");
    }
}

#[test]
fn only_a_renamed_row_with_a_known_version_is_scoped_to_its_line() {
    assert!(in_row_scope(true, Some("0.52.0"), "0.52.3"));
    assert!(!in_row_scope(true, Some("0.52.0"), "0.61.2"));
    assert!(!in_row_scope(true, Some("0.52.0"), "0.61"));
    assert!(in_row_scope(true, Some("0.52.0"), "0"));
    assert!(in_row_scope(true, Some("0.52.0"), "0.0"));
    assert!(in_row_scope(true, Some("0.0.3"), "0"));
    assert!(in_row_scope(true, Some("0.0.3"), "0.0"));
    assert!(in_row_scope(true, Some("0.0.3"), "0.0.3"));
    assert!(!in_row_scope(true, Some("0.0.3"), "0.0.4"));
    assert!(in_row_scope(false, Some("0.52.0"), "0.61.2"));
    assert!(in_row_scope(true, None, "0.61.2"));
}
