// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_cli_harness_names_itself_and_reads_its_own_version() {
    for harness in CliHarness::ALL {
        let ver = harness.version();
        assert!(ver.tuple() >= (0, 1), "{harness:?}");
        assert_eq!(ver.as_str(), harness.version_str(), "{harness:?}");
        assert_eq!(
            ver.as_str(),
            format!("{}.{}", ver.major(), ver.minor()),
            "{harness:?}: the text is the numbers"
        );
        assert_eq!(
            HarnessVersion::parse_major_minor(ver.as_str()),
            Some(ver.tuple()),
            "{harness:?}"
        );
        assert_eq!(CliHarness::for_name(harness.id()), Some(harness));
        assert_eq!(CliHarness::for_name(harness.cli()), Some(harness));
        assert_eq!(CliHarness::for_name(harness.product()), Some(harness));
        let sources = harness.sources();
        assert_eq!(sources.first().copied(), Some(harness.version_file()));
        assert!(
            sources.len() >= 2,
            "{harness:?}: owns its reader and its harness files"
        );
    }
    let dir = aterm_tempfile::manifest_dir().join("../..");
    for (i, a) in CliHarness::ALL.iter().enumerate() {
        for &src in a.sources() {
            assert!(
                dir.join(src).is_file(),
                "{a:?}: source path {src} must exist as a file"
            );
        }
        for b in &CliHarness::ALL[i + 1..] {
            for &sa in a.sources() {
                for &sb in b.sources() {
                    assert!(
                        sa != sb && !sa.starts_with(sb) && !sb.starts_with(sa),
                        "{a:?} and {b:?} must have disjoint sources, shared {sa:?} / {sb:?}"
                    );
                }
            }
        }
    }
    for (name, harness) in [
        ("grok build", CliHarness::Grok),
        (" \tGrOk BuIlD\n", CliHarness::Grok),
        ("Kimi Code", CliHarness::Kimi),
    ] {
        assert_eq!(CliHarness::for_name(name), Some(harness), "{name:?}");
    }
    for retired in [
        "gemini",
        "gemini-cli",
        "Gemini CLI",
        "codex",
        "codex-alab",
        "codex-cli",
        "codex-old",
    ] {
        assert_eq!(
            CliHarness::for_name(retired),
            None,
            "{retired:?}: agy replaces it"
        );
    }
}

#[test]
fn each_version_file_holds_its_one_line() {
    let dir = aterm_tempfile::manifest_dir().join("../..");
    for harness in CliHarness::ALL {
        let text = std::fs::read_to_string(dir.join(harness.version_file()))
            .unwrap_or_else(|e| panic!("{harness:?}: {e}"));
        let line = format!(
            concat!(
                "pub const HARNESS_VER: crate::HarnessVersion = ",
                "crate::HarnessVersion::new({}, {}, \"{}\");"
            ),
            harness.version().major(),
            harness.version().minor(),
            harness.version_str()
        );
        assert_eq!(text.matches(&line).count(), 1, "{harness:?}: {line}");
    }
}

#[test]
fn numeric_version_ordering_holds() {
    let v0_1 = HarnessVersion::new(0, 1, "0.1");
    let v0_2 = HarnessVersion::new(0, 2, "0.2");
    let v0_10 = HarnessVersion::new(0, 10, "0.10");
    let v0_1000 = HarnessVersion::new(0, 1_000, "0.1000");
    let v1_0 = HarnessVersion::new(1, 0, "1.0");
    let max_minor = HarnessVersion::new(0, u16::MAX, "0.65535");
    assert!(v0_1 < v0_2);
    assert!(v0_2 < v0_10, "numeric ordering, not lexicographic");
    assert!(v0_10 < v0_1000);
    assert!(v0_1000 < v1_0);
    assert!(max_minor < v1_0);
    assert!(std::panic::catch_unwind(|| HarnessVersion::new(0, 1, "v0.1")).is_err());
    assert!(std::panic::catch_unwind(|| HarnessVersion::new(0, 0, "0.0")).is_err());
    assert!(std::panic::catch_unwind(|| HarnessVersion::new(0, 2, "0.1")).is_err());
}

#[test]
fn parsing_takes_two_parts_and_never_a_third() {
    assert_eq!(HarnessVersion::parse_major_minor("0.1"), Some((0, 1)));
    assert_eq!(HarnessVersion::parse_major_minor("v0.1"), Some((0, 1)));
    assert_eq!(HarnessVersion::parse_major_minor("0.10"), Some((0, 10)));
    assert_eq!(
        HarnessVersion::parse_major_minor("65535.65535"),
        Some((u16::MAX, u16::MAX))
    );
    for invalid in [
        "0.1.0",
        "0.4.1",
        "0.01",
        "00.1",
        "",
        "65536.1",
        "1.65536",
        "999999999999999999999.1",
        "1.999999999999999999999",
        " 0.1",
        "0.1 ",
        "+0.1",
        "0.-1",
        "v",
        "0.1+gsource",
        "0.1-dev.1",
    ] {
        assert_eq!(
            HarnessVersion::parse_major_minor(invalid),
            None,
            "{invalid}"
        );
    }
}

#[test]
fn a_cli_version_is_cut_to_its_numbers() {
    for (raw, core) in [
        ("2.1.305", Some("2.1.305")),
        ("v2.1.310", Some("2.1.310")),
        ("1.4.13-R6996.1", Some("1.4.13")),
        ("0.165.0-dev.4+g1a2b3c4d5", Some("0.165.0")),
        ("0.1.0-dev.3+codex.0.165.0.g1a2b3c4d5", Some("0.1.0")),
        (" 2.1.305 ", Some("2.1.305")),
        ("", None),
        ("?", None),
        ("latest", None),
        ("-1", None),
        ("2.1.", None),
        ("2..1", None),
        (".2.1", None),
        ("2.1.305.", None),
        ("v", None),
    ] {
        assert_eq!(cli_version_core(raw), core, "{raw:?}");
    }
    assert!(
        !cli_version_later("2.1.", "2.1.0") && !cli_version_later("2.1.0", "2.1."),
        "a malformed core is no version on either side"
    );
    assert!(cli_version_later("2.1.310", "2.1.305"));
    assert!(cli_version_later("2.1.310", "2.1.31"), "numeric, not text");
    assert!(cli_version_later("2.2", "2.1.999"));
    assert!(cli_version_later("1.4.14-R7000.1", "1.4.13-R6996.1"));
    assert!(!cli_version_later("2.1.305", "2.1.305"));
    assert!(!cli_version_later("2.1.305", "2.1.310"));
    assert!(!cli_version_later("2.1.305.0", "2.1.305"));
    assert!(!cli_version_later("latest", "2.1.305"));
    assert!(!cli_version_later("2.1.310", ""));
}

/// A LATER REVISION OR DEV BUILD UNDER ONE CORE IS OFFERED, NEVER A STEP BACK: a later core,
/// a later Muse revision and a later dev count under the same core are moves the ⬆️ offers,
/// with the build past the core named; the same build, an earlier core, an earlier revision
/// or dev count under the same core, builds of other shapes and a side that is not a version
/// are not. CONTROL: `cli_version_later` alone refuses the revision.
#[test]
fn a_revision_or_dev_build_under_one_core_is_offered_never_a_step_back() {
    for (to, from) in [
        ("2.1.310", "2.1.305"),
        ("1.4.10-R6500.1", "1.4.10-R6427.1"),
        ("1.4.10-R6427.1", "1.4.10-R6427"),
        ("1.4.10-R6427.10", "1.4.10-R6427.9"),
        ("0.164.0-dev.12521+gabc", "0.164.0-dev.12365+gdef"),
        (
            "0.1.0-dev.13765+codex.0.164.0.g5d4c3b2a1",
            "0.1.0-dev.13764+codex.0.164.0.g4b3a367a0",
        ),
        ("1.4.11-R6500.1", "1.4.10-R6427.1"),
    ] {
        assert!(cli_version_offered(to, from), "{from} -> {to}");
    }
    for (to, from) in [
        ("2.1.305", "2.1.305"),
        ("v2.1.305", "2.1.305"),
        ("2.1.305", "2.1.310"),
        ("1.4.9-R6999.1", "1.4.10-R6427.1"),
        ("1.4.10-R6427.1", "1.4.10-R6500.1"),
        ("1.4.10-R6427", "1.4.10-R6427.1"),
        ("1.4.10-R06427.1", "1.4.10-R6427.1"),
        ("0.164.0-dev.12365+gdef", "0.164.0-dev.12521+gabc"),
        (
            "0.1.0-dev.13764+codex.0.164.0.g4b3a367a0",
            "0.1.0-dev.13765+codex.0.164.0.g5d4c3b2a1",
        ),
        (
            "0.1.0-dev.13764+codex.0.165.0.g5d4c3b2a1",
            "0.1.0-dev.13764+codex.0.164.0.g4b3a367a0",
        ),
        ("1.4.10-R6500.1", "1.4.10-dev.3"),
        ("1.4.10-R6500.1", "1.4.10"),
        ("latest", "2.1.305"),
        ("2.1.310", ""),
        ("2.1.", "2.1.0"),
    ] {
        assert!(!cli_version_offered(to, from), "{from} -> {to}");
    }
    assert!(
        !cli_version_later("1.4.10-R6500.1", "1.4.10-R6427.1"),
        "CONTROL: the cores alone are equal"
    );
    assert!(cli_version_later(
        "1.4.99999999999999999999999999",
        "1.4.99999999999999999999999998"
    ));
    assert!(!cli_version_offered(
        "1.4.99999999999999999999999998-R6500.1",
        "1.4.99999999999999999999999999-R6427.1"
    ));
    for (raw, build) in [
        ("1.4.10-R6500.1", Some("R6500.1")),
        ("0.164.0-dev.12521+g1a2b3c4d5", Some("dev.12521")),
        ("0.1.0-dev.3+codex.0.165.0.g1a2", Some("dev.3")),
        ("2.1.305", None),
        ("2.1.305+gabc-1", None),
        ("1.4.10-", None),
        ("latest-1", None),
    ] {
        assert_eq!(cli_version_build(raw), build, "{raw:?}");
    }
}

#[test]
fn a_numeric_build_difference_still_checks_the_remaining_paired_shape() {
    for (to, from) in [
        ("1.4.10-R6500.foo", "1.4.10-R6400.bar"),
        ("1.4.10-R6500.2-extra", "1.4.10-R6400.1-other"),
        ("0.1.0-dev.13765.alpha", "0.1.0-dev.13764.beta"),
    ] {
        assert!(
            !cli_version_offered(to, from),
            "different later shape: {from} -> {to}"
        );
        assert!(
            !cli_version_offered(from, to),
            "different earlier shape: {to} -> {from}"
        );
    }
    for (to, from) in [
        ("1.4.10-R6500.1", "1.4.10-R6427.9"),
        ("1.4.10-R6500", "1.4.10-R6427.9"),
    ] {
        assert!(
            cli_version_offered(to, from),
            "compatible shape: {from} -> {to}"
        );
        assert!(!cli_version_offered(from, to), "CONTROL: reverse ordering");
    }
}
