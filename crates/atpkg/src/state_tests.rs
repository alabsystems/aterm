// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The first `; ` of a held row is the seam between the failure and the retry, even
/// when the failure's own words carry one (a signer's free text): the pending stub
/// splits there, and a seam inside the failure would print half of it as the retry.
#[test]
fn a_held_rows_first_seam_is_between_the_failure_and_the_retry() {
    let how = "retried when the pin changes; now: aterm pkg install claude";
    let row = held_failed("signer refused: not signed; team X", how);
    assert_eq!(
        row,
        "held: last attempt failed with signer refused: not signed, team X; retried \
         when the pin changes; now: aterm pkg install claude"
    );
    let (why, rest) = row
        .strip_prefix(HELD_FAILED_PREFIX)
        .and_then(|held| held.split_once("; "))
        .unwrap();
    assert_eq!(
        (why, rest),
        ("signer refused: not signed, team X", how),
        "the retry comes back whole, its own `; ` and all"
    );
}

/// A row's source and why it is not current are read off the canonical spellings, so the
/// package log, the Activity list and the program rows say what `doctor` says.
#[test]
fn a_rows_source_and_why_it_is_not_current_come_from_its_canonical_words() {
    assert_eq!(
        source_words("claude", &vendor_managed("2.1.280", "Anthropic"), 44),
        "Anthropic latest"
    );
    assert_eq!(source_words("codex", "error: x", 0), "OpenAI latest");
    assert_eq!(source_words("ay", &managed(1971, 41), 44), "ALab index 41");
    assert_eq!(
        source_words("ay", "error: stage failed", 44),
        "ALab index 44"
    );
    assert_eq!(source_words("ay", "active", 0), "ALab index");

    for current in [
        managed(1971, 41),
        vendor_managed("2.1.280", "Anthropic"),
        vendor_source("0.156.0", "OpenAI"),
        shadowed(1971, Path::new("/opt/homebrew/bin/ay")),
        system(Path::new("/usr/bin/git"), None),
        agent_installing(),
        String::from("extra — not installed (opt in: aterm pkg install vendorx)"),
        String::from("needs admin — run: aterm pkg install clt"),
        String::from("blocked by clt: needs admin — run: aterm pkg install clt"),
        String::from("active"),
        String::new(),
    ] {
        assert_eq!(not_current(&current), None, "{current}");
    }
    let held = vendor_kept("2.1.280", "held by local pin");
    assert_eq!(not_current(&held), Some(("held", "held by local pin")));
    let yanked = vendor_kept("2.1.279", "Anthropic's latest (2.1.281) is yanked");
    assert_eq!(not_current(&yanked).map(|n| n.0), Some("yanked"));
    let unreached = vendor_kept(
        "2.1.280",
        "Anthropic's release channel not reached since 2026-09-20",
    );
    assert_eq!(not_current(&unreached).map(|n| n.0), Some("not reached"));
    let rolled = vendor_kept("2.1.279", "rolled back from yanked 2.1.280");
    assert_eq!(not_current(&rolled).map(|n| n.0), Some("rolled back"));
    let refused = "error: claude 2.1.281 refused: digest mismatch — keeping 2.1.280";
    assert_eq!(not_current(refused), Some(("refused", refused)));
    for refusal in [
        "error: codex refused: no release for this target",
        "error: manifest refused: retired kind",
        "error: artifact row refused: url host is evil",
        "error: stage: signer refused: team id differs",
        "error: the signed row for ay was refused: url host is evil",
    ] {
        assert_eq!(
            not_current(refusal).map(|n| n.0),
            Some("refused"),
            "{refusal}"
        );
    }
    for failure in [
        "error: fetch https://downloads.claude.ai/x: Connection refused (os error 61)",
        "error: download: proxy connection refused: tunnel closed",
        "error: claude 2.1.281: connection refused",
        "error: someprogram 1.2.3 refused: not a vendor lane line",
    ] {
        assert_eq!(
            not_current(failure).map(|n| n.0),
            Some("failed"),
            "{failure}"
        );
    }
    assert_eq!(
        not_current("error: stage: disk full").map(|n| n.0),
        Some("failed")
    );
    assert_eq!(not_current(TOMBSTONED_PIN).map(|n| n.0), Some("disabled"));
    assert_eq!(
        not_current(&held_unpublished(None, 12, "x86_64-apple-darwin", 11)).map(|n| n.0),
        Some("held")
    );
    assert_eq!(
        not_current(TOOLSET_INDEX_UNREACHABLE).map(|n| n.0),
        Some("unavailable")
    );
}

/// A vendor row is managed, names the version and the vendor, and is no index pin.
#[test]
fn a_vendor_row_is_managed_by_version_and_vendor() {
    let row = vendor_managed("2.1.280", "Anthropic");
    assert_eq!(row, "managed 2.1.280 — Anthropic latest");
    assert!(is_managed(&row));
    assert_eq!(managed_pin(&row), None);
    assert_eq!(shadowed_by(&row), None);
    assert_eq!(vendor_row(&row), Some(("2.1.280", "Anthropic latest")));
    assert_eq!(vendor_latest(&row), Some(("2.1.280", "Anthropic")));
    let kept = vendor_kept("2.1.280", "held by local pin");
    assert_eq!(kept, "managed 2.1.280 — held by local pin");
    assert!(is_managed(&kept) && !kept.contains("latest"));
    assert_eq!(managed_pin(&kept), None);
    assert_eq!(vendor_row(&kept), Some(("2.1.280", "held by local pin")));
    assert_eq!(vendor_latest(&kept), None);
    let source = vendor_source("0.156.0", "OpenAI");
    assert_eq!(source, "managed 0.156.0 — updates from OpenAI");
    assert_eq!(vendor_latest(&source), None);
    let muse = vendor_source("1.4.9-R6297.1", "Meta");
    assert_eq!(
        vendor_row(&muse),
        Some(("1.4.9-R6297.1", "updates from Meta"))
    );
    let newer = vendor_kept("2.1.281", "newer than Anthropic's latest (2.1.280)");
    assert_eq!(vendor_latest(&newer), None);
    assert_eq!(vendor_latest("managed 2.1.280 — Nobody latest"), None);
    for other in [
        managed(6808, 41),
        shadowed(6808, Path::new("/p")),
        shadowed(
            crate::vendor_direct::Version::parse("2.1.280")
                .unwrap()
                .build_id(),
            Path::new("/p"),
        ),
        String::from("managed 2.1 — Anthropic latest"),
        String::from("managed 1.4.9-r6297 — updates from Meta"),
        String::from("managed 2.1.280 — "),
    ] {
        assert_eq!(vendor_row(&other), None, "{other}");
        assert_eq!(vendor_latest(&other), None, "{other}");
    }
}

/// A vendor build's SHADOWED rows name its version, never its 19-digit store id, and
/// the machine-wide one reads back as that id.
#[test]
fn a_vendor_shadow_row_names_the_version_and_reads_back_the_build() {
    let build = crate::vendor_direct::Version::parse("2.1.280")
        .unwrap()
        .build_id();
    let row = shadowed(build, Path::new("/Users//dev/.local/bin/claude"));
    assert_eq!(
        row,
        "managed 2.1.280 — SHADOWED by /Users//dev/.local/bin/claude"
    );
    assert_eq!(
        shadowed_by(&row),
        Some((build, "/Users//dev/.local/bin/claude"))
    );
    let shell = agent_shadowed_in_shell(
        build,
        Path::new("/Users//dev/.local/bin/claude"),
        Path::new("/p/agents"),
        false,
        ". ~/.aterm/shell.d/00-atpkg.zsh",
    );
    assert!(shell.starts_with("managed 2.1.280 — SHADOWED in this shell by "));
    for s in [&row, &shell] {
        assert!(!s.contains(&build.to_string()), "{s}");
    }
    assert_eq!(shadowed_by("managed 2.1 — SHADOWED by /p"), None);
    let muse = crate::vendor_direct::Version::parse("1.4.9-R6297.1")
        .unwrap()
        .build_id();
    let row = shadowed(muse, Path::new("/p/muse"));
    assert_eq!(row, "managed 1.4.9-R6297.1 — SHADOWED by /p/muse");
    assert_eq!(shadowed_by(&row), Some((muse, "/p/muse")));
}

/// The agent programs' shell-local shadow row is a managed row, and NOT a `SHADOWED by`
/// row to the parser, because it is one shell's state, not the machine's.
#[test]
fn the_agent_shadow_row_is_never_the_machine_state() {
    let agents = Path::new("/Users//dev/Library/Application Support/aterm/pkg/agents");
    let absent = agent_shadowed_in_shell(
        2_026_091_601,
        Path::new("/Users//dev/.local/bin/claude"),
        agents,
        false,
        ". ~/.aterm/shell.d/00-atpkg.zsh",
    );
    let behind = agent_shadowed_in_shell(
        2_026_091_601,
        Path::new("/opt/homebrew/bin/codex"),
        agents,
        true,
        "source ~/.aterm/shell.d/00-atpkg.fish",
    );
    for row in [&absent, &behind] {
        assert!(is_managed(row), "{row}");
        assert_eq!(shadowed_by(row), None, "not the machine-wide row: {row}");
        assert_eq!(managed_pin(row), None, "{row}");
        for fault in [
            "error:",
            "unavailable:",
            "blocked:",
            "aborted:",
            "tombstoned:",
        ] {
            assert!(!row.starts_with(fault), "{row}");
        }
    }
}

#[test]
fn the_parsers_read_back_exactly_what_the_constructors_wrote() {
    let plain = system(Path::new("/opt/homebrew/bin/gh"), None);
    assert_eq!(system_path(&plain), Some("/opt/homebrew/bin/gh"));
    assert_eq!(system_retired(&plain), None);
    let noted = system(Path::new("/opt/homebrew/bin/gh"), Some("2026-08-27"));
    assert_eq!(system_path(&noted), Some("/opt/homebrew/bin/gh"));
    assert_eq!(system_retired(&noted), Some("2026-08-27"));
    let odd = system(Path::new("/x — not managed by aterm/gh"), None);
    assert_eq!(system_path(&odd), Some("/x"));
    assert_eq!(managed_pin(&managed(6808, 41)), Some((6808, 41)));
    assert_eq!(managed_pin("managed +6808 — pinned by index 41"), None);
    assert_eq!(managed_pin("managed 06808 — pinned by index 41"), None);
    assert_eq!(managed_pin("managed 6808 — pinned by index +41"), None);
    assert_eq!(managed_pin(&shadowed(6808, Path::new("/x/trust"))), None);
    assert_eq!(
        shadowed_by(&shadowed(6808, Path::new("/x/trust"))),
        Some((6808, "/x/trust"))
    );
    assert_eq!(shadowed_by("managed +6808 — SHADOWED by /x/trust"), None);
    assert_eq!(shadowed_by("managed 06808 — SHADOWED by /x/trust"), None);
    assert_eq!(shadowed_by(&managed(6808, 41)), None);
    assert!(is_managed(&managed(1, 1)) && is_managed(&shadowed(1, Path::new("/p"))));
    for other in [
        "active",
        "error: x",
        &unavailable("t", ""),
        &held_unpublished(None, 6, "t", 5),
        &held_unpublished(Some("tb"), 6, "t", 3),
    ] {
        assert_eq!(system_path(other), None, "{other}");
        assert_eq!(managed_pin(other), None, "{other}");
        assert!(!is_managed(other), "{other}");
    }
}

/// Doctor prefix-matches its faults (`error:`, `unavailable:`, `blocked:`, `aborted:`,
/// `tombstoned:`); no canonical spelling may start with one, or a normal row would read
/// as a problem.
#[test]
fn no_canonical_state_reads_as_a_doctor_fault() {
    for s in [
        managed(1, 1),
        vendor_managed("2.1.280", "Anthropic"),
        vendor_kept("2.1.280", "held by local pin"),
        vendor_source("0.156.0", "OpenAI"),
        system(Path::new("/p"), Some("2026-01-01")),
        shadowed(1, Path::new("/p")),
        unavailable("t", "h"),
        held_unpublished(None, 6, "t", 5),
        held_unpublished(Some("tb"), 6, "t", 3),
    ] {
        for fault in [
            "error:",
            "unavailable:",
            "blocked:",
            "aborted:",
            "tombstoned:",
        ] {
            assert!(!s.starts_with(fault), "{s} would read as a {fault} fault");
        }
    }
}
